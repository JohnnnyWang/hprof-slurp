pub mod args;
pub mod errors;
pub mod parser;
pub mod prefetch_reader;
pub mod result_recorder;
pub mod slurp;
pub mod utils;

use crate::args::get_args;
use crate::errors::HprofSlurpError;
use crate::slurp::slurp_file;
use std::time::Instant;
