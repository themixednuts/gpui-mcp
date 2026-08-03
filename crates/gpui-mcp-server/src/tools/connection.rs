use super::{
    BridgeResult, GpuiMcp, Json, Operation, ToolRouter, Value, json, object_output, tool,
    tool_router,
};

#[tool_router(router = connection_router)]
impl GpuiMcp {
    #[tool(
        description = "Check the GPUI bridge connection and return application identity and capabilities"
    )]
    async fn ping(&self) -> Result<Json<Value>, String> {
        let result = self.client.call(Operation::Ping).await?;
        let BridgeResult::Pong {
            app_id,
            pid,
            protocol_version,
        } = result
        else {
            return Err("bridge returned the wrong result for ping".to_owned());
        };
        Ok(object_output(json!({
            "ok": true,
            "app_id": app_id,
            "pid": pid,
            "protocol_version": protocol_version,
            "capabilities": self.client.descriptor().capabilities,
        })))
    }

    #[tool(description = "Alias for ping; diagnose whether the configured GPUI endpoint is live")]
    async fn check_connection(&self) -> Result<Json<Value>, String> {
        self.ping().await
    }
}

pub(super) fn router() -> ToolRouter<GpuiMcp> {
    GpuiMcp::connection_router()
}
