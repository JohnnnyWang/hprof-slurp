use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::sync::Arc;

use ahash::AHashMap;
use indicatif::{ProgressBar, ProgressStyle};

use crossbeam_channel::{Receiver, Sender};
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};

use crate::errors::HprofSlurpError;
use crate::errors::HprofSlurpError::*;
use crate::parser::file_header_parser::{parse_file_header, FileHeader};
use crate::parser::gc_record::{ClassDumpFields, FieldType, FieldValue, GcRecord};
use crate::parser::record::Record;
use crate::parser::record_parser::parse_field_value;
use crate::parser::record_stream_parser::HprofRecordStreamParser;
use crate::prefetch_reader::PrefetchReader;
use crate::result_recorder::{Instance, RenderedResult, ResultRecorder};
use crate::utils::pretty_bytes_size;

// the exact size of the file header (31 bytes)
const FILE_HEADER_LENGTH: usize = 31;

// 64 MB buffer performs nicely (higher is faster but increases the memory consumption)
pub const READ_BUFFER_SIZE: usize = 64 * 1024 * 1024;

pub fn slurp_file(file_path: String) -> Result<ResultRecorder, HprofSlurpError> {
    let file = File::open(file_path)?;
    let file_len = file.metadata()?.len() as usize;
    let mut reader = BufReader::new(file);

    // Parse file header
    let header = slurp_header(&mut reader)?;
    let id_size = header.size_pointers;
    println!(
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

    pb.set_position(99);
    pb.finish_and_clear();
    // Wait for final result
    let mut result = receive_result
        .recv()
        .expect("result channel should be alive");

    parse_instance(&mut result);
    // Blocks until pre-fetcher is done
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

fn parse_instance(result: &mut ResultRecorder) {
    let instance: HashMap<u64, Arc<Instance>> = result
        .dump_instances
        .par_iter()
        .map(|ele| {
            if let GcRecord::InstanceDump {
                object_id,
                stack_trace_serial_number,
                class_object_id,
                data_size,
                data_bytes,
            } = ele
            {
                if let Some(class) = result.classes_dump.get(class_object_id) {
                    let (a, b) = parse_instance_data(class, data_bytes, result);

                    let instance = Instance {
                        object_id: *object_id,
                        stack_trace_serial_number: *stack_trace_serial_number,
                        class_object_id: *class_object_id,
                        data_size: *data_size,
                        fields: a,
                        super_fields: b,
                    };
                    Some((*object_id, Arc::new(instance)))
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
    println!("parse instance done, instance cnt: {}", instance.len());
}

fn parse_instance_data(
    class: &ClassDumpFields,
    data_bytes: &[u8],
    result: &ResultRecorder,
) -> (AHashMap<String, FieldValue>, AHashMap<String, FieldValue>) {
    let mut data_pt = data_bytes;
    let mut fields_with_name: AHashMap<String, FieldValue> = AHashMap::new();
    let mut super_fields_with_name: AHashMap<String, FieldValue> = AHashMap::new();
    for fields in &class.instance_fields {
        let name = if let Some(field_name) = result.utf8_strings_by_id.get(&fields.name_id) {
            field_name.to_string()
        } else {
            "UNKNOWN".to_string()
        };

        let parser = parse_field_value(fields.field_type);
        let (remaining, value) = parser(data_pt).unwrap();
        data_pt = remaining;
        fields_with_name.insert(name, value);
    }

    //super class, merged
    if let Some(super_class) = result.classes_dump.get(&class.super_class_object_id) {
        let (this, s) = parse_instance_data(super_class, data_pt, result);
        super_fields_with_name.extend(this);
        super_fields_with_name.extend(s);
    }

    (fields_with_name, super_fields_with_name)
}
