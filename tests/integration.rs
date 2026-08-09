// Keep integration tests in one crate so libtest can run every module in parallel.
mod cli;
mod graph_signal_ablation_report;
mod model_ab_trajectory_report;
mod process;
mod resolved_reference_oracle_report;
mod services;
