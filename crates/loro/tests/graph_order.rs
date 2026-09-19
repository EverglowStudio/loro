//! LG05/LG06 deterministic contracts and an independent, operation-set oracle.
//! The oracle is written before the native cases and never imports Loro code.

#[path = "graph_order_model/basic.rs"]
mod basic;
#[path = "graph_order_model/boundaries.rs"]
mod boundaries;
#[path = "graph_order_model/collisions.rs"]
mod collisions;
#[path = "graph_order_model/concurrency.rs"]
mod concurrency;
#[path = "graph_order_model/copy_diff.rs"]
mod copy_diff;
#[path = "graph_order_model/copy_diff_cases.rs"]
mod copy_diff_cases;
#[path = "graph_order_model/copy_diff_fixture.rs"]
mod copy_diff_fixture;
#[path = "graph_order_model/copy_diff_planned.rs"]
mod copy_diff_planned;
#[path = "graph_order_model/driver.rs"]
mod driver;
#[path = "graph_order_model/events.rs"]
mod events;
#[path = "graph_order_model/history.rs"]
mod history;
#[path = "graph_order_model/lifecycle.rs"]
mod lifecycle;
#[path = "graph_order_model/oracle.rs"]
mod oracle;
#[path = "graph_order_model/oracle_cases.rs"]
mod oracle_cases;
#[path = "graph_order_model/random.rs"]
mod random;
#[path = "graph_order_model/replay.rs"]
mod replay;
#[path = "graph_order_model/replay_driver.rs"]
mod replay_driver;
#[path = "graph_order_model/support.rs"]
mod support;
#[path = "graph_order_model/undo.rs"]
mod undo;
