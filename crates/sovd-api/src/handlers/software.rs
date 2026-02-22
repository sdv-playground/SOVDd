//! Software handlers
//!
//! This module is kept for backward compatibility but most functionality
//! has been moved to the async flash flow (files + flash handlers).
//!
//! Upload (reading from ECU) functionality can be added back if needed
//! using a similar async pattern.

// Module is currently empty - upload handlers removed as part of async flash refactoring.
// The new flow uses:
// - POST /files to upload package
// - POST /files/:id/verify to verify
// - POST /flash/transfer to start async flashing
// - GET /flash/transfer/:id to poll status
// - PUT /flash/transferexit to finalize
