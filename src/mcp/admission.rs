use super::*;

#[derive(Debug, Clone)]
pub(in crate::mcp) struct RequestAdmission {
    pub(in crate::mcp) active: Arc<tokio::sync::Semaphore>,
}

impl RequestAdmission {
    pub(in crate::mcp) fn new(active_capacity: usize) -> Self {
        Self {
            active: Arc::new(tokio::sync::Semaphore::new(active_capacity)),
        }
    }

    pub(in crate::mcp) fn try_admit(&self) -> crate::Result<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.active)
            .try_acquire_owned()
            .map_err(|_| crate::Error::RetrievalOverloaded)
    }

    #[cfg(test)]
    pub(in crate::mcp) fn available_permits(&self) -> usize {
        self.active.available_permits()
    }
}
