use rmcp::ServiceExt;

use crate::error::BmError;
use super::BmMcpServer;

/// Serve the MCP server over stdio until the client disconnects.
pub async fn serve_stdio(server: BmMcpServer) -> Result<(), BmError> {
    let transport = rmcp::transport::stdio();
    let running = server
        .serve(transport)
        .await
        .map_err(|e| BmError::Other(format!("mcp server failed to start: {e}")))?;
    running
        .waiting()
        .await
        .map_err(|e| BmError::Other(format!("mcp server stopped abnormally: {e}")))?;
    Ok(())
}
