pub mod externalpodautoscaler;
mod run;

#[cfg(test)]
mod run_test;

pub use run::run_all;
