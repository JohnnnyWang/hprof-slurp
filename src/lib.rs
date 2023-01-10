use std::{sync::Arc, collections::HashMap};

use ahash::AHashMap;
use parser::{
    gc_record::ClassDumpFields,
    record::{LoadClassData, StackFrameData, StackTraceData},
};
use result_recorder::{Instance, ResultRecorder};

pub mod args;
pub mod errors;
pub mod parser;
pub mod prefetch_reader;
pub mod result_recorder;
pub mod slurp;
pub mod utils;

#[derive(Debug, Clone)]
pub struct Heap {
    pub counter: HeapCounter,

    pub utf8_strings: HashMap<u64, Box<str>>,
    pub class_data: HashMap<u64, LoadClassData>,
    pub classes_dump: HashMap<u64, ClassDumpFields>,
    pub stack_trace_by_serial_number: HashMap<u32, StackTraceData>,
    pub stack_frame_by_id: HashMap<u64, StackFrameData>,
    pub instances_pool: HashMap<u64, Arc<Instance>>,
}
#[derive(Debug, Clone)]
pub struct HeapCounter {
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
}

impl From<ResultRecorder> for Heap {
    fn from(value: ResultRecorder) -> Self {
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
            heap_dump_segments_gc_root_thread_object: value
                .heap_dump_segments_gc_root_thread_object,
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
        Self {
            counter,
            utf8_strings: value.utf8_strings_by_id,
            class_data: value.load_class,
            classes_dump: value.classes_dump,
            stack_trace_by_serial_number: value.stack_trace_by_serial_number,
            stack_frame_by_id: value.stack_frame_by_id,
            instances_pool: value.instances,
        }
    }
}
