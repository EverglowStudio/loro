//! Independent graph specification and three-replica differential tests.
//!
//! The oracle only sees test actions and their causal ancestors. It neither
//! imports GraphOp/GraphState nor reads production lifecycle tags or indexes.
//! See graph_model/README.md for the driver contract and replay commands.

#[path = "graph_model/differential.rs"]
mod differential;
#[path = "graph_model/driver.rs"]
mod driver;
#[path = "graph_model/native.rs"]
mod native;
#[path = "graph_model/oracle.rs"]
mod oracle;
#[path = "graph_model/oracle_cases.rs"]
mod oracle_cases;
#[path = "graph_model/regressions.rs"]
mod regressions;
