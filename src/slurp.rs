use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::sync::Arc;

use indicatif::{ProgressBar, ProgressStyle};

use crossbeam_channel::{Receiver, Sender};
use log::info;
use rayon::prelude::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator};

use crate::errors::HprofSlurpError;
use crate::errors::HprofSlurpError::*;
use crate::parser::file_header_parser::{parse_file_header, FileHeader};
use crate::parser::gc_record::{ClassDumpFields, GcRecord, Values};
use crate::parser::record::Record;
use crate::parser::record_parser::{parse_array_value, parse_field_value};
use crate::parser::record_stream_parser::HprofRecordStreamParser;
use crate::prefetch_reader::PrefetchReader;
use crate::result_recorder::{Instance, ResultRecorder};
use crate::utils::pretty_bytes_size;
use crate::{Heap, HeapCounter};

// the exact size of the file header (31 bytes)
const FILE_HEADER_LENGTH: usize = 31;

// 64 MB buffer performs nicely (higher is faster but increases the memory consumption)
pub const READ_BUFFER_SIZE: usize = 128 * 1024 * 1024;

pub fn slurp_file(file_path: String) -> Result<Heap, HprofSlurpError> {
    let file = File::open(file_path)?;
    let file_len = file.metadata()?.len() as usize;
    let mut reader = BufReader::new(file);

    // Parse file header
    let header = slurp_header(&mut reader)?;
    let id_size = header.size_pointers;
    info!(
        "Processing {} binary hprof file in '{}' format.",
        pretty_bytes_size(file_len as u64),
        header.format
    );

    // Communication channel from pre-fetcher to parser
    let (send_data, receive_data): (Sender<Vec<u8>>, Receiver<Vec<u8>>) =
        crossbeam_channel::unbounded();

    // Communication channel from parser to pre-fetcher (pooled input buffers)
    let (send_pooled_data, receive_pooled_data): (Sender<Vec<u8>>, Receiver<Vec<u8>>) =
        crossbeam_channel::unbounded();

    // Init pooled binary data with more than 1 element to enable the reader to make progress interdependently
    for _ in 0..2 {
        send_pooled_data
            .send(Vec::with_capacity(READ_BUFFER_SIZE))
            .expect("pre-fetcher channel should be alive");
    }

    // Communication channel from parser to recorder
    let (send_records, receive_records): (Sender<Vec<Record>>, Receiver<Vec<Record>>) =
        crossbeam_channel::unbounded();

    // Communication channel from recorder to parser (pooled record buffers)
    let (send_pooled_vec, receive_pooled_vec): (Sender<Vec<Record>>, Receiver<Vec<Record>>) =
        crossbeam_channel::unbounded();

    // Communication channel from recorder to main
    let (send_result, receive_result): (Sender<ResultRecorder>, Receiver<ResultRecorder>) =
        crossbeam_channel::unbounded();

    // Communication channel from parser to main
    let (send_progress, receive_progress): (Sender<usize>, Receiver<usize>) =
        crossbeam_channel::unbounded();

    // Init pre-fetcher
    let prefetcher = PrefetchReader::new(reader, file_len, FILE_HEADER_LENGTH, READ_BUFFER_SIZE);
    let prefetch_thread = prefetcher.start(send_data, receive_pooled_data)?;

    // Init pooled result vec
    send_pooled_vec
        .send(Vec::new())
        .expect("recorder channel should be alive");

    // Init stream parser
    let initial_loop_buffer = Vec::with_capacity(READ_BUFFER_SIZE); // will be added to the data pool after the first chunk
    let stream_parser =
        HprofRecordStreamParser::new(file_len, FILE_HEADER_LENGTH, initial_loop_buffer);

    // Start stream parser
    let parser_thread = stream_parser.start(
        receive_data,
        send_pooled_data,
        send_progress,
        receive_pooled_vec,
        send_records,
    )?;

    // Init result recorder
    let result_recorder = ResultRecorder::new(id_size);
    let recorder_thread = result_recorder.start(receive_records, send_result, send_pooled_vec)?;

    // Init progress bar
    let pb = ProgressBar::new(file_len as u64);
    pb.set_style(ProgressStyle::default_bar()
        .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} (speed:{bytes_per_sec}) (eta:{eta})")
        .expect("templating should never fail")
        .progress_chars("#>-"));

    // Feed progress bar
    while let Ok(processed) = receive_progress.recv() {
        pb.set_position(processed as u64)
    }
    prefetch_thread
        .join()
        .map_err(|e| HprofSlurpError::StdThreadError { e })?;

    // Blocks until parser is done
    parser_thread
        .join()
        .map_err(|e| HprofSlurpError::StdThreadError { e })?;

    // Blocks until recorder is done
    recorder_thread
        .join()
        .map_err(|e| HprofSlurpError::StdThreadError { e })?;

    pb.set_position(99);
    pb.finish_and_clear();
    // Wait for final result
    let result = receive_result
        .recv()
        .expect("result channel should be alive");

    let result = parse_instance(result);

    // parser_vm_overview(&result);

    // Blocks until pre-fetcher is done

    // Finish and remove progress bar

    Ok(result)
}

//TODO: support 32bits
pub fn slurp_header(reader: &mut BufReader<File>) -> Result<FileHeader, HprofSlurpError> {
    let mut header_buffer = vec![0; FILE_HEADER_LENGTH];
    reader.read_exact(&mut header_buffer)?;
    let (rest, header) = parse_file_header(&header_buffer).map_err(|e| InvalidHprofFile {
        message: format!("{:?}", e),
    })?;
    // Invariants
    let id_size = header.size_pointers;
    if id_size != 4 && id_size != 8 {
        return Err(InvalidIdSize);
    }
    if id_size == 4 {
        return Err(UnsupportedIdSize {
            message: "32 bits heap dumps are not supported yet".to_string(),
        });
    }
    if !rest.is_empty() {
        return Err(InvalidHeaderSize);
    }
    Ok(header)
}

fn parser_vm_overview(result: &ResultRecorder) {
    //start up time
    if let Some(str_id) = search_str("sun.management.ManagementFactoryHelper", result) {
        if let Some(cdf) = search_dump_class(str_id, result) {
            println!("{:?}", cdf);
        } else {
            println!("UNKNOWN: sun.management.ManagementFactoryHelper");
        }
    }

    if let Some(str_id) = search_str("sun.management.ManagementFactory", result) {
        if let Some(cdf) = search_dump_class(str_id, result) {
            println!("{:?}", cdf);
        } else {
            println!("UNKNOWN: sun.management.ManagementFactory");
        }
    }
}

fn parse_instance(value: ResultRecorder) -> Heap {
    let mut heap = Heap::default();

    let counter = HeapCounter {
        id_size: value.id_size,
        classes_unloaded: value.classes_unloaded,
        stack_frames: value.stack_frames,
        stack_traces: value.stack_traces,
        start_threads: value.start_threads,
        end_threads: value.end_threads,
        heap_summaries: value.heap_summaries,
        heap_dumps: value.heap_dumps,
        allocation_sites: value.allocation_sites,
        control_settings: value.control_settings,
        cpu_samples: value.cpu_samples,
        heap_dump_segments_all_sub_records: value.heap_dump_segments_all_sub_records,
        heap_dump_segments_gc_root_unknown: value.heap_dump_segments_gc_root_unknown,
        heap_dump_segments_gc_root_thread_object: value.heap_dump_segments_gc_root_thread_object,
        heap_dump_segments_gc_root_jni_global: value.heap_dump_segments_gc_root_jni_global,
        heap_dump_segments_gc_root_jni_local: value.heap_dump_segments_gc_root_jni_local,
        heap_dump_segments_gc_root_java_frame: value.heap_dump_segments_gc_root_java_frame,
        heap_dump_segments_gc_root_native_stack: value.heap_dump_segments_gc_root_native_stack,
        heap_dump_segments_gc_root_sticky_class: value.heap_dump_segments_gc_root_sticky_class,
        heap_dump_segments_gc_root_thread_block: value.heap_dump_segments_gc_root_thread_block,
        heap_dump_segments_gc_root_monitor_used: value.heap_dump_segments_gc_root_monitor_used,
        heap_dump_segments_gc_object_array_dump: value.heap_dump_segments_gc_object_array_dump,
        heap_dump_segments_gc_instance_dump: value.heap_dump_segments_gc_instance_dump,
        heap_dump_segments_gc_primitive_array_dump: value
            .heap_dump_segments_gc_primitive_array_dump,
        heap_dump_segments_gc_class_dump: value.heap_dump_segments_gc_class_dump,
    };

    heap.counter = counter;

    let instance: HashMap<u64, Arc<Instance>> = value
        .dump_instances
        .into_par_iter()
        .map(|ele| {
            if let GcRecord::InstanceDump {
                object_id,
                stack_trace_serial_number,
                class_object_id,
                data_size,
                bytes_ref,
            } = ele
            {
                if let Some(class) = value.classes_dump.get(&class_object_id) {
                    let (a, b) = parse_instance_data(
                        class,
                        &bytes_ref,
                        &value.utf8_strings_by_id,
                        &value.classes_dump,
                    );

                    let instance = Instance {
                        object_id,
                        stack_trace_serial_number,
                        class_object_id,
                        data_size,
                        fields: a,
                        super_fields: b,
                    };
                    Some((object_id, Arc::new(instance)))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .filter(|e| e.is_some())
        .map(|e| e.unwrap())
        .collect();

    let instance_primitive_array_dump: HashMap<u64, Arc<Instance>> = value
        .dump_primitive_array_dump
        .into_par_iter()
        .map(|ele| {
            if let GcRecord::PrimitiveArrayDump {
                object_id,
                stack_trace_serial_number,
                number_of_elements,
                element_type,
                bytes_ref,
            } = ele
            {
                let (_, value) =
                    parse_array_value(element_type.clone(), number_of_elements)(&bytes_ref)
                        .unwrap();
                let mut fields = Vec::default();
                fields.push((0, Values::Array(value)));

                let instance = Instance {
                    object_id,
                    stack_trace_serial_number,
                    class_object_id: element_type.to_u64(),
                    data_size: bytes_ref.len() as u32,
                    fields,
                    super_fields: Vec::default(),
                };
                drop(bytes_ref);
                Some((object_id, Arc::new(instance)))
            } else {
                None
            }
        })
        .filter(|e| e.is_some())
        .map(|e| e.unwrap())
        .collect();

    let instance_object_array_dump: HashMap<u64, Arc<Instance>> = value
        .dump_object_array_dump
        .into_par_iter()
        .map(|ele| {
            if let GcRecord::ObjectArrayDump {
                object_id,
                stack_trace_serial_number,
                number_of_elements,
                array_class_id,
                bytes_ref,
            } = ele
            {
                let (_, value) = parse_array_value(
                    crate::parser::gc_record::FieldType::Object,
                    number_of_elements,
                )(&bytes_ref)
                .unwrap();
                let mut fields = Vec::default();
                fields.push((0, Values::Array(value)));

                let instance = Instance {
                    object_id,
                    stack_trace_serial_number,
                    class_object_id: array_class_id,
                    data_size: bytes_ref.len() as u32,
                    fields,
                    super_fields: Vec::with_capacity(0),
                };

                drop(bytes_ref);
                Some((object_id, Arc::new(instance)))
            } else {
                None
            }
        })
        .filter(|e| e.is_some())
        .map(|e| e.unwrap())
        .collect();
    heap.instances_pool.extend(instance);
    heap.instances_pool.extend(instance_primitive_array_dump);
    heap.instances_pool.extend(instance_object_array_dump);

    heap.utf8_strings = value.utf8_strings_by_id;
    heap.class_data = value.load_class;
    heap.classes_dump = value.classes_dump;
    heap.stack_frame_by_id = value.stack_frame_by_id;
    heap.stack_trace_by_serial_number = value.stack_trace_by_serial_number;
    heap.root_jni_global = value.root_jni_global;
    heap.root_jni_local = value.root_jni_local;
    heap.root_thread_object = value.root_thread_object;

    heap
}

fn parse_instance_data(
    class: &ClassDumpFields,
    data_bytes: &[u8],
    _utf8_strings_by_id: &HashMap<u64, Box<str>>,
    _classes_dump: &HashMap<u64, ClassDumpFields>,
) -> (Vec<(u64, Values)>, Vec<(u64, Values)>) {
    let mut data_pt = data_bytes;
    let mut fields_with_name: Vec<(u64, Values)> = Vec::with_capacity(class.instance_fields.len());
    let super_fields_with_name: Vec<(u64, Values)> = Vec::new();
    for field in &class.instance_fields {
        // let name = if let Some(field_name) = utf8_strings_by_id.get(&fields.name_id) {
        //     field_name.to_string()
        // } else {
        //     "UNKNOWN".to_string()
        // };

        let parser = parse_field_value(field.field_type);
        let (remaining, value) = parser(data_pt).unwrap();
        data_pt = remaining;
        fields_with_name.push((field.name_id, Values::Single(value)));
    }

    //super class, merged
    // if let Some(super_class) = classes_dump.get(&class.super_class_object_id) {
    //     let (this, s) = parse_instance_data(super_class, data_pt, utf8_strings_by_id, classes_dump);
    //     super_fields_with_name.extend(this);
    //     super_fields_with_name.extend(s);
    // }

    (fields_with_name, super_fields_with_name)
}

fn search_str(str: &str, result: &ResultRecorder) -> Option<u64> {
    if let Some((id, _)) = result
        .utf8_strings_by_id
        .par_iter()
        .find_first(|(_, v)| v.contains(str))
    {
        Some(*id)
    } else {
        None
    }
}

fn search_dump_class(name_str_id: u64, result: &ResultRecorder) -> Option<ClassDumpFields> {
    if let Some((_k, v)) = result
        .load_class
        .par_iter()
        .find_first(|(_k, v)| v.class_name_id == name_str_id)
    {
        let obj_id = v.class_object_id;
        if let Some((_, cdf)) = result
            .classes_dump
            .par_iter()
            .find_first(|(_k, v)| v.class_object_id == obj_id)
        {
            Some(cdf.clone())
        } else {
            None
        }
    } else {
        None
    }
}
