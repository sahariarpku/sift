//! Reporting (§8.11): flow diagram, checklist, manuscript content, automation
//! disclosure, RIS and CSV exports. The DOCX file is assembled in the app
//! window with the `docx` package from `manuscript::build`.

pub mod apa;
pub mod exports;
pub mod flow;
pub mod manuscript;
pub mod metrics;
