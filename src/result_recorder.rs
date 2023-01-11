use ahash::AHashMap;
use crossbeam_channel::{Receiver, Sender};
use indoc::formatdoc;

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::{mem, thread};

use crate::parser::gc_record::*;
use crate::parser::record::{LoadClassData, Record, StackFrameData, StackTraceData};
use crate::parser::record::{Record::*, ThreadEndData, ThreadStartData};
use crate::utils::pretty_bytes_size;

#[derive(Debug, Copy, Clone)]
pub struct ClassInfo {
    super_class_object_id: u64,
    instance_size: u32,
}

impl ClassInfo {
    fn new(super_class_object_id: u64, instance_size: u32) -> Self {
        Self {
            super_class_object_id,
            instance_size,
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct ClassInstanceCounter {
    number_of_instances: u64,
}

impl ClassInstanceCounter {
    pub fn add_instance(&mut self) {
        self.number_of_instances += 1;
    }

    pub fn empty() -> ClassInstanceCounter {
        ClassInstanceCounter {
            number_of_instances: 0,
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct ArrayCounter {
    number_of_arrays: u64,
    max_size_seen: u32,
    total_number_of_elements: u64,
}

impl ArrayCounter {
    pub fn add_elements_from_array(&mut self, elements: u32) {
        self.number_of_arrays += 1;
        self.total_number_of_elements += elements as u64;
        if elements > self.max_size_seen {
            self.max_size_seen = elements
        }
    }

    pub fn empty() -> ArrayCounter {
        ArrayCounter {
            number_of_arrays: 0,
            total_number_of_elements: 0,
            max_size_seen: 0,
        }
    }
}

pub struct RenderedResult {
    pub summary: String,
    pub thread_info: String,
    pub memory_usage: String,
    pub captured_strings: Option<String>,
}
#[derive(Debug, Clone)]
pub struct Instance {
    pub object_id: u64,
    pub stack_trace_serial_number: u32,
    pub class_object_id: u64,
    pub data_size: u32,
    pub fields: Vec<(u64, Values)>,
    pub super_fields: Vec<(u64, Values)>,
}

pub struct ResultRecorder {
    pub id_size: u32,
    // Tag counters
    pub classes_unloaded: i32,
    pub stack_frames: i32,
    pub stack_traces: i32,
    pub start_threads: i32,
    pub end_threads: i32,
    pub heap_summaries: i32,
    pub heap_dumps: i32,
    pub allocation_sites: i32,
    pub control_settings: i32,
    pub cpu_samples: i32,
    // GC tag counters
    pub heap_dump_segments_all_sub_records: i32,
    pub heap_dump_segments_gc_root_unknown: i32,
    pub heap_dump_segments_gc_root_thread_object: i32,
    pub heap_dump_segments_gc_root_jni_global: i32,
    pub heap_dump_segments_gc_root_jni_local: i32,
    pub heap_dump_segments_gc_root_java_frame: i32,
    pub heap_dump_segments_gc_root_native_stack: i32,
    pub heap_dump_segments_gc_root_sticky_class: i32,
    pub heap_dump_segments_gc_root_thread_block: i32,
    pub heap_dump_segments_gc_root_monitor_used: i32,
    pub heap_dump_segments_gc_object_array_dump: i32,
    pub heap_dump_segments_gc_instance_dump: i32,
    pub heap_dump_segments_gc_primitive_array_dump: i32,
    pub heap_dump_segments_gc_class_dump: i32,
    // Captured state
    // "object_id" -> "class_id" -> "class_name_id" -> "utf8_string"
    pub utf8_strings_by_id: HashMap<u64, Box<str>>,
    pub class_data: Vec<LoadClassData>,        // holds class_data
    pub class_data_by_id: HashMap<u64, usize>, // value is index into class_data
    pub class_data_by_serial_number: HashMap<u32, usize>, // value is index into class_data
    pub classes_single_instance_size_by_id: HashMap<u64, ClassInfo>,
    pub classes_dump: HashMap<u64, ClassDumpFields>,
    pub classes_all_instance_total_size_by_id: HashMap<u64, ClassInstanceCounter>,
    pub primitive_array_counters: HashMap<FieldType, ArrayCounter>,
    pub object_array_counters: HashMap<u64, ArrayCounter>,
    pub stack_trace_by_serial_number: HashMap<u32, StackTraceData>,
    pub stack_frame_by_id: HashMap<u64, StackFrameData>,

    //add
    pub dump_instances: Vec<GcRecord>,
    pub dump_primitive_array_dump: Vec<GcRecord>,
    pub dump_object_array_dump: Vec<GcRecord>,
    pub instances: HashMap<u64, Arc<Instance>>,

    pub load_class: HashMap<u64, LoadClassData>,

    pub thread_start: HashMap<u32, ThreadStartData>,
    pub thread_end: HashMap<u32, ThreadEndData>,
}

impl ResultRecorder {
    pub fn new(id_size: u32) -> Self {
        ResultRecorder {
            id_size,
            classes_unloaded: 0,
            stack_frames: 0,
            stack_traces: 0,
            start_threads: 0,
            end_threads: 0,
            heap_summaries: 0,
            heap_dumps: 0,
            allocation_sites: 0,
            control_settings: 0,
            cpu_samples: 0,
            heap_dump_segments_all_sub_records: 0,
            heap_dump_segments_gc_root_unknown: 0,
            heap_dump_segments_gc_root_thread_object: 0,
            heap_dump_segments_gc_root_jni_global: 0,
            heap_dump_segments_gc_root_jni_local: 0,
            heap_dump_segments_gc_root_java_frame: 0,
            heap_dump_segments_gc_root_native_stack: 0,
            heap_dump_segments_gc_root_sticky_class: 0,
            heap_dump_segments_gc_root_thread_block: 0,
            heap_dump_segments_gc_root_monitor_used: 0,
            heap_dump_segments_gc_object_array_dump: 0,
            heap_dump_segments_gc_primitive_array_dump: 0,
            heap_dump_segments_gc_instance_dump: 0,
            heap_dump_segments_gc_class_dump: 0,
            utf8_strings_by_id: HashMap::new(),
            class_data: vec![],
            class_data_by_id: HashMap::new(),
            class_data_by_serial_number: HashMap::default(),
            classes_single_instance_size_by_id: HashMap::new(),
            classes_all_instance_total_size_by_id: HashMap::new(),
            primitive_array_counters: HashMap::new(),
            object_array_counters: HashMap::new(),
            classes_dump: HashMap::default(),
            stack_trace_by_serial_number: HashMap::default(),
            stack_frame_by_id: HashMap::default(),
            dump_instances: Vec::default(),
            dump_primitive_array_dump: Vec::default(),
            instances: HashMap::default(),
            load_class: HashMap::default(),
            thread_start: HashMap::default(),
            thread_end: HashMap::default(),
            dump_object_array_dump: Vec::default(),
        }
    }

    fn get_class_name_string(&self, class_id: &u64) -> String {
        self.class_data_by_id
            .get(class_id)
            .and_then(|data_index| self.class_data.get(*data_index))
            .and_then(|class_data| self.utf8_strings_by_id.get(&class_data.class_name_id))
            .expect("class_id must have an UTF-8 string representation available")
            .replace('/', ".")
    }

    pub fn start(
        mut self,
        receive_records: Receiver<Vec<Record>>,
        send_result: Sender<Self>,
        send_pooled_vec: Sender<Vec<Record>>,
    ) -> std::io::Result<JoinHandle<()>> {
        thread::Builder::new()
            .name("hprof-recorder".to_string())
            .spawn(move || {
                loop {
                    match receive_records.recv() {
                        Ok(mut records) => {
                            self.record_records(&mut records);
                            // clear values but retain underlying storage
                            records.clear();
                            // send back pooled vec (swallow errors as it is possible the receiver was already dropped)
                            send_pooled_vec.send(records).unwrap_or_default();
                        }
                        Err(_) => {
                            // no more Record to pull, generate and send back results

                            send_result
                                .send(self)
                                .expect("channel should not be closed");
                            break;
                        }
                    }
                }
            })
    }

    fn record_records(&mut self, records: &mut [Record]) {
        records.iter_mut().for_each(|record| match record {
            Utf8String { id, str } => {
                self.utf8_strings_by_id.insert(*id, mem::take(str));
            }
            LoadClass(load_class_data) => {
                let class_object_id = load_class_data.class_object_id;
                // let class_serial_number = load_class_data.serial_number;
                // self.class_data.push(mem::take(load_class_data));
                // let data_index = self.class_data.len() - 1;
                // self.class_data_by_id.insert(class_object_id, data_index);
                // self.class_data_by_serial_number
                //     .insert(class_serial_number, data_index);

                self.load_class
                    .insert(class_object_id, load_class_data.clone());
            }
            UnloadClass { .. } => self.classes_unloaded += 1,
            StackFrame(stack_frame_data) => {
                self.stack_frames += 1;
                self.stack_frame_by_id
                    .insert(stack_frame_data.stack_frame_id, mem::take(stack_frame_data));
            }
            StackTrace(stack_trace_data) => {
                self.stack_traces += 1;
                self.stack_trace_by_serial_number
                    .insert(stack_trace_data.serial_number, mem::take(stack_trace_data));
            }
            StartThread {
                thread_serial_number,
                thread_object_id,
                stack_trace_serial_number,
                thread_name_id,
                thread_group_name_id,
                thread_group_parent_name_id,
            } => {
                self.thread_start.insert(
                    *thread_serial_number,
                    ThreadStartData {
                        thread_serial_number: *thread_serial_number,
                        thread_object_id: *thread_object_id,
                        stack_trace_serial_number: *stack_trace_serial_number,
                        thread_name_id: *thread_name_id,
                        thread_group_name_id: *thread_group_name_id,
                        thread_group_parent_name_id: *thread_group_parent_name_id,
                    },
                );
            }
            EndThread {
                thread_serial_number,
            } => {
                self.thread_end.insert(
                    *thread_serial_number,
                    ThreadEndData {
                        thread_serial_number: *thread_serial_number,
                    },
                );
            }
            AllocationSites { .. } => self.allocation_sites += 1,
            HeapSummary {
                total_live_bytes,
                total_live_instances,
                total_bytes_allocated,
                total_instances_allocated,
            } => self.heap_summaries += 1,
            ControlSettings { .. } => self.control_settings += 1,
            CpuSamples { .. } => self.cpu_samples += 1,
            HeapDumpEnd { .. } => (),
            HeapDumpStart { .. } => self.heap_dumps += 1,
            GcSegment(gc_record) => {
                self.heap_dump_segments_all_sub_records += 1;
                match gc_record {
                    GcRecord::RootUnknown { .. } => self.heap_dump_segments_gc_root_unknown += 1,
                    GcRecord::RootThreadObject { .. } => {
                        self.heap_dump_segments_gc_root_thread_object += 1
                    }
                    GcRecord::RootJniGlobal { .. } => {
                        self.heap_dump_segments_gc_root_jni_global += 1
                    }
                    GcRecord::RootJniLocal { .. } => self.heap_dump_segments_gc_root_jni_local += 1,
                    GcRecord::RootJavaFrame { .. } => {
                        self.heap_dump_segments_gc_root_java_frame += 1
                    }
                    GcRecord::RootNativeStack { .. } => {
                        self.heap_dump_segments_gc_root_native_stack += 1
                    }
                    GcRecord::RootStickyClass { .. } => {
                        self.heap_dump_segments_gc_root_sticky_class += 1
                    }
                    GcRecord::RootThreadBlock { .. } => {
                        self.heap_dump_segments_gc_root_thread_block += 1
                    }
                    GcRecord::RootMonitorUsed { .. } => {
                        self.heap_dump_segments_gc_root_monitor_used += 1
                    }
                    GcRecord::InstanceDump {
                        object_id,
                        stack_trace_serial_number,
                        class_object_id,
                        data_size,
                        bytes_ref,
                    } => {
                        self.classes_all_instance_total_size_by_id
                            .entry(*class_object_id)
                            .or_insert_with(ClassInstanceCounter::empty)
                            .add_instance();

                        self.heap_dump_segments_gc_instance_dump += 1;
                        self.dump_instances.push(GcRecord::InstanceDump {
                            object_id: *object_id,
                            stack_trace_serial_number: *stack_trace_serial_number,
                            class_object_id: *class_object_id,
                            data_size: *data_size,
                            bytes_ref: bytes_ref.clone(),
                        });
                    }
                    GcRecord::ObjectArrayDump {
                        number_of_elements,
                        array_class_id,
                        object_id,
                        stack_trace_serial_number,
                        bytes_ref,
                    } => {
                        self.object_array_counters
                            .entry(*array_class_id)
                            .or_insert_with(ArrayCounter::empty)
                            .add_elements_from_array(*number_of_elements);

                        self.dump_object_array_dump.push(GcRecord::ObjectArrayDump {
                            number_of_elements: *number_of_elements,
                            array_class_id: *array_class_id,
                            object_id: *object_id,
                            stack_trace_serial_number: *stack_trace_serial_number,
                            bytes_ref: bytes_ref.clone(),
                        });
                        self.heap_dump_segments_gc_object_array_dump += 1
                    }
                    GcRecord::PrimitiveArrayDump {
                        number_of_elements,
                        element_type,
                        object_id,
                        stack_trace_serial_number,
                        bytes_ref,
                    } => {
                        self.primitive_array_counters
                            .entry(*element_type)
                            .or_insert_with(ArrayCounter::empty)
                            .add_elements_from_array(*number_of_elements);

                        self.heap_dump_segments_gc_primitive_array_dump += 1;

                        self.dump_primitive_array_dump
                            .push(GcRecord::PrimitiveArrayDump {
                                number_of_elements: *number_of_elements,
                                element_type: *element_type,
                                object_id: *object_id,
                                stack_trace_serial_number: *stack_trace_serial_number,
                                bytes_ref: bytes_ref.clone(),
                            });
                    }
                    GcRecord::ClassDump(class_dump_fields) => {
                        let class_object_id = class_dump_fields.class_object_id;
                        self.classes_dump
                            .insert(class_object_id, *(*class_dump_fields).clone());
                        self.classes_single_instance_size_by_id
                            .entry(class_object_id)
                            .or_insert_with(|| {
                                let instance_size = class_dump_fields.instance_size;
                                let super_class_object_id = class_dump_fields.super_class_object_id;
                                ClassInfo::new(super_class_object_id, instance_size)
                            });

                        self.heap_dump_segments_gc_class_dump += 1
                    }
                }
            }
        });
    }

    fn render_captured_strings(&self) -> String {
        let mut strings: Vec<_> = self.utf8_strings_by_id.values().collect();
        strings.sort();
        let mut result = String::new();
        result.push_str("\nList of Strings\n");
        strings.iter().for_each(|s| {
            result.push_str(s);
            result.push('\n')
        });
        result
    }

    fn render_thread_info(&self) -> String {
        let mut thread_info = String::new();

        // for each stacktrace
        let mut stack_traces: Vec<_> = self
            .stack_trace_by_serial_number
            .iter()
            .filter(|(_, stack)| !stack.stack_frame_ids.is_empty()) // omit empty stacktraces
            .collect();

        stack_traces.sort_by_key(|(serial_number, _)| **serial_number);

        thread_info.push_str(&format!(
            "\nFound {} threads with stacktraces:\n",
            stack_traces.len()
        ));

        for (index, (_id, stack_data)) in stack_traces.iter().enumerate() {
            thread_info.push_str(&format!("\nThread {}\n", index + 1));

            //  for each stack frames
            for stack_frame_id in &stack_data.stack_frame_ids {
                let stack_frame = self.stack_frame_by_id.get(stack_frame_id).unwrap();
                let class_object_id = self
                    .class_data_by_serial_number
                    .get(&stack_frame.class_serial_number)
                    .and_then(|index| self.class_data.get(*index))
                    .expect("Class not found")
                    .class_object_id;
                let class_name = self.get_class_name_string(&class_object_id);
                let method_name = self
                    .utf8_strings_by_id
                    .get(&stack_frame.method_name_id)
                    .map(|b| b.deref())
                    .unwrap_or("unknown method name");
                let file_name = self
                    .utf8_strings_by_id
                    .get(&stack_frame.source_file_name_id)
                    .map(|b| b.deref())
                    .unwrap_or("unknown source file");

                // >0: normal
                // -1: unknown
                // -2: compiled method
                // -3: native method
                let pretty_line_number = match stack_frame.line_number {
                    -1 => "unknown line number".to_string(),
                    -2 => "compiled method".to_string(),
                    -3 => "native method".to_string(),
                    number => format!("{}", number),
                };

                // pretty frame output
                let stack_frame_pretty = format!(
                    "  at {}.{} ({}:{})\n",
                    class_name, method_name, file_name, pretty_line_number
                );
                thread_info.push_str(&stack_frame_pretty);
            }
        }
        thread_info
    }

    fn render_memory_usage(&self) -> String {
        // https://www.baeldung.com/java-memory-layout
        // total_size = object_header + data
        // on a 64-bit arch.
        // object_header = mark(ref_size) + klass(4) + padding_gap(4) = 16 bytes
        // data = instance_size + padding_next(??)
        let object_header = self.id_size + 4 + 4;

        let mut classes_dump_vec: Vec<_> = self
            .classes_all_instance_total_size_by_id
            .iter()
            .map(|(class_id, v)| {
                let class_name = self.get_class_name_string(class_id);
                let mut size = 0;

                let ClassInfo {
                    super_class_object_id,
                    instance_size,
                } = self
                    .classes_single_instance_size_by_id
                    .get(class_id)
                    .unwrap();
                let mut parent_class_id = *super_class_object_id;
                size += instance_size;

                // recursively add sizes from parent classes
                while parent_class_id != 0 {
                    let ClassInfo {
                        super_class_object_id,
                        instance_size,
                    } = self
                        .classes_single_instance_size_by_id
                        .get(&parent_class_id)
                        .unwrap();
                    size += instance_size;
                    parent_class_id = *super_class_object_id;
                }
                // add object header
                size += object_header;
                // add extra padding if any
                size += size.rem_euclid(8);
                let total_size = size as u64 * v.number_of_instances;
                (
                    class_name,
                    v.number_of_instances,
                    size as u64, // all instances have the same size
                    total_size,
                )
            })
            .collect();

        // https://www.baeldung.com/java-memory-layout
        // the array's `elements` size is already accounted for via `GcInstanceDump` for objects
        // unlike primitives which are packed in the array itself
        // array headers already aligned for 64-bit arch - no need for padding
        // array_header = mark(ref_size) + klass(4) + array_length(4) = 16 bytes
        // data_primitive = primitive_size * length + padding(??)
        // data_object = ref_size * length (no padding because the ref size is already aligned!)
        let ref_size = self.id_size as u64;
        let array_header_size = ref_size + 4 + 4;

        let array_primitives_dump_vec = self.primitive_array_counters.iter().map(|(ft, &ac)| {
            let primitive_type = format!("{:?}", ft).to_lowercase();
            let primitive_array_label = format!("{}[]", primitive_type);
            let primitive_size = primitive_byte_size(ft);

            let cost_of_all_array_headers = array_header_size * ac.number_of_arrays;
            let cost_of_all_values = primitive_size * ac.total_number_of_elements;
            // info lost at this point to compute the real padding for each array
            // assume mid value of 4 bytes per array for an estimation
            let estimated_cost_of_all_padding = ac.number_of_arrays * 4;

            let cost_data_largest_array = primitive_size * ac.max_size_seen as u64;
            let cost_padding_largest_array =
                (array_header_size + cost_data_largest_array).rem_euclid(8);
            (
                primitive_array_label,
                ac.number_of_arrays,
                array_header_size + cost_data_largest_array + cost_padding_largest_array,
                cost_of_all_array_headers + cost_of_all_values + estimated_cost_of_all_padding,
            )
        });

        // For array of objects we are interested in the total size of the array headers and outgoing elements references
        let array_objects_dump_vec = self.object_array_counters.iter().map(|(class_id, &ac)| {
            let raw_class_name = self.get_class_name_string(class_id);
            let cleaned_class_name: String = if raw_class_name.starts_with("[L") {
                // remove '[L' prefix and ';' suffix
                raw_class_name
                    .chars()
                    .skip(2)
                    .take(raw_class_name.chars().count() - 3)
                    .collect()
            } else if raw_class_name.starts_with("[[L") {
                // remove '[[L' prefix and ';' suffix
                raw_class_name
                    .chars()
                    .skip(3)
                    .take(raw_class_name.chars().count() - 4)
                    .collect()
            } else {
                // TODO: what are those ([[C, [[D, [[B, [[S ...)? boxed primitives are already present
                raw_class_name
            };

            let object_array_label = format!("{}[]", cleaned_class_name);

            let cost_of_all_refs = ref_size * ac.total_number_of_elements;
            let cost_of_all_array_headers = array_header_size * ac.number_of_arrays;
            let cost_of_largest_array_refs = ref_size * ac.max_size_seen as u64;
            (
                object_array_label,
                ac.number_of_arrays,
                array_header_size + cost_of_largest_array_refs,
                cost_of_all_array_headers + cost_of_all_refs,
            )
        });

        // Merge results
        classes_dump_vec.extend(array_primitives_dump_vec);
        classes_dump_vec.extend(array_objects_dump_vec);

        // Holds the final result
        let mut analysis = String::new();

        // Total heap size found banner
        let total_size = classes_dump_vec.iter().map(|(_, _, _, s)| *s).sum();
        let display_total_size = pretty_bytes_size(total_size);
        let allocation_classes_title = format!(
            "Found a total of {} of instances allocated on the heap.\n",
            display_total_size
        );
        analysis.push_str(&allocation_classes_title);

        // Sort by class name first for stability in test results :s
        classes_dump_vec.sort_by(|a, b| b.0.cmp(&a.0));

        // Top allocated classes analysis
        // let allocation_classes_title = format!("\nTop {} allocated classes:\n\n", top);
        analysis.push_str(&allocation_classes_title);
        classes_dump_vec.sort_by(|a, b| b.3.cmp(&a.3));
        // ResultRecorder::render_table(self.top, &mut analysis, classes_dump_vec.as_slice());

        // Top largest instances analysis
        // let allocation_largest_title = format!("\nTop {} largest instances:\n\n", top);
        // analysis.push_str(&allocation_largest_title);
        classes_dump_vec.sort_by(|a, b| b.2.cmp(&a.2));
        // ResultRecorder::render_table(self.top, &mut analysis, classes_dump_vec.as_slice());

        analysis
    }

    // Render table from [(class_name, count, largest_allocation, instance_size)]
    fn render_table(top: usize, analysis: &mut String, rows: &[(String, u64, u64, u64)]) {
        let rows_formatted: Vec<_> = rows
            .iter()
            .take(top)
            .map(|(class_name, count, largest_allocation, allocation_size)| {
                let display_allocation = pretty_bytes_size(*allocation_size);
                let largest_display_allocation = pretty_bytes_size(*largest_allocation);
                (
                    display_allocation,
                    *count,
                    largest_display_allocation,
                    class_name,
                )
            })
            .collect();

        let total_size_header = "Total size";
        let total_size_header_padding = ResultRecorder::padding_for_header(
            rows_formatted.as_slice(),
            |r| r.0.to_string(),
            total_size_header,
        );
        let total_size_len =
            total_size_header.chars().count() + total_size_header_padding.chars().count();

        let instance_count_header = "Instances";
        let instance_count_header_padding = ResultRecorder::padding_for_header(
            rows_formatted.as_slice(),
            |r| r.1.to_string(),
            instance_count_header,
        );
        let instance_len =
            instance_count_header.chars().count() + instance_count_header_padding.chars().count();

        let largest_instance_header = "Largest";
        let largest_instance_padding = ResultRecorder::padding_for_header(
            rows_formatted.as_slice(),
            |r| r.2.to_string(),
            largest_instance_header,
        );
        let largest_len =
            largest_instance_header.chars().count() + largest_instance_padding.chars().count();

        let class_name_header = "Class name";
        let class_name_padding = ResultRecorder::padding_for_header(
            rows_formatted.as_slice(),
            |r| r.3.to_string(),
            class_name_header,
        );

        let header = format!(
            "{}{} | {}{} | {}{} | {}{}\n",
            total_size_header_padding,
            total_size_header,
            instance_count_header_padding,
            instance_count_header,
            largest_instance_padding,
            largest_instance_header,
            class_name_header,
            class_name_padding
        );
        let header_len = header.chars().count();
        analysis.push_str(&header);
        analysis.push_str(&("-".repeat(header_len)));
        analysis.push('\n');

        rows_formatted.into_iter().for_each(
            |(allocation_size, count, largest_allocation_size, class_name)| {
                let padding_size_str =
                    ResultRecorder::column_padding(&allocation_size, total_size_len);
                let padding_count_str =
                    ResultRecorder::column_padding(&count.to_string(), instance_len);
                let padding_largest_size_str =
                    ResultRecorder::column_padding(&largest_allocation_size, largest_len);

                let row = format!(
                    "{}{} | {}{} | {}{} | {}\n",
                    padding_size_str,
                    allocation_size,
                    padding_count_str,
                    count,
                    padding_largest_size_str,
                    largest_allocation_size,
                    class_name
                );
                analysis.push_str(&row);
            },
        );
    }

    fn padding_for_header<F>(
        rows: &[(String, u64, String, &String)],
        field_selector: F,
        header_label: &str,
    ) -> String
    where
        F: Fn(&(String, u64, String, &String)) -> String,
    {
        let max_elem_size = rows
            .iter()
            .map(|d| field_selector(d).chars().count())
            .max_by(|x, y| x.cmp(y))
            .expect("Results can't be empty");

        ResultRecorder::column_padding(header_label, max_elem_size)
    }

    fn column_padding(column_name: &str, max_item_length: usize) -> String {
        let column_label_len = column_name.chars().count();
        let padding_size = if max_item_length > column_label_len {
            max_item_length - column_label_len
        } else {
            0
        };
        " ".repeat(padding_size)
    }

    pub fn render_summary(&self) -> String {
        let top_summary = formatdoc!(
            "\nFile content summary:\n
            UTF-8 Strings: {}
            Classes loaded: {}
            Classes unloaded: {}
            Stack traces: {}
            Stack frames: {}
            Start threads: {}
            Allocation sites: {}
            End threads: {}
            Control settings: {}
            CPU samples: {}",
            self.utf8_strings_by_id.len(),
            self.class_data_by_id.len(),
            self.classes_unloaded,
            self.stack_traces,
            self.stack_frames,
            self.start_threads,
            self.allocation_sites,
            self.end_threads,
            self.control_settings,
            self.cpu_samples
        );

        let heap_summary = formatdoc!(
            "Heap summaries: {}
            {} heap dumps containing in total {} segments:
            ..GC root unknown: {}
            ..GC root thread objects: {}
            ..GC root JNI global: {}
            ..GC root JNI local: {}
            ..GC root Java frame: {}
            ..GC root native stack: {}
            ..GC root sticky class: {}
            ..GC root thread block: {}
            ..GC root monitor used: {}
            ..GC primitive array dump: {}
            ..GC object array dump: {}
            ..GC class dump: {}
            ..GC instance dump: {}",
            self.heap_summaries,
            self.heap_dumps,
            self.heap_dump_segments_all_sub_records,
            self.heap_dump_segments_gc_root_unknown,
            self.heap_dump_segments_gc_root_thread_object,
            self.heap_dump_segments_gc_root_jni_global,
            self.heap_dump_segments_gc_root_jni_local,
            self.heap_dump_segments_gc_root_java_frame,
            self.heap_dump_segments_gc_root_native_stack,
            self.heap_dump_segments_gc_root_sticky_class,
            self.heap_dump_segments_gc_root_thread_block,
            self.heap_dump_segments_gc_root_monitor_used,
            self.heap_dump_segments_gc_primitive_array_dump,
            self.heap_dump_segments_gc_object_array_dump,
            self.heap_dump_segments_gc_class_dump,
            self.heap_dump_segments_gc_instance_dump,
        );

        format!("{}\n{}", top_summary, heap_summary)
    }
}

fn primitive_byte_size(field_type: &FieldType) -> u64 {
    match field_type {
        FieldType::Byte | FieldType::Bool => 1,
        FieldType::Char | FieldType::Short => 2,
        FieldType::Float | FieldType::Int => 4,
        FieldType::Double | FieldType::Long => 8,
        FieldType::Object => panic!("object type in primitive array"),
    }
}
