mod server;
#[cfg(test)]
mod server_test;

pub use server::{Readiness, serve};
