pub mod protocol;
pub mod socket;

use protocol::{AgentInfo, ProcessInfo};

pub trait HerdrApi {
    type Error;

    fn list_agents(&mut self) -> Result<Vec<AgentInfo>, Self::Error>;
    fn process_info(&mut self, pane_id: &str) -> Result<ProcessInfo, Self::Error>;
    fn report_metadata(
        &mut self,
        pane_id: &str,
        applies_to_source: Option<&str>,
        seq: u64,
        ttl_ms: u64,
        session_name: Option<&str>,
        last_message: Option<&str>,
    ) -> Result<(), Self::Error>;
}

impl HerdrApi for socket::SocketTransport {
    type Error = socket::SocketError;

    fn list_agents(&mut self) -> Result<Vec<AgentInfo>, Self::Error> {
        self.list_agents()
    }

    fn process_info(&mut self, pane_id: &str) -> Result<ProcessInfo, Self::Error> {
        self.process_info(pane_id)
    }

    fn report_metadata(
        &mut self,
        pane_id: &str,
        applies_to_source: Option<&str>,
        seq: u64,
        ttl_ms: u64,
        session_name: Option<&str>,
        last_message: Option<&str>,
    ) -> Result<(), Self::Error> {
        self.report_metadata(
            pane_id,
            applies_to_source,
            seq,
            ttl_ms,
            session_name,
            last_message,
        )
    }
}
