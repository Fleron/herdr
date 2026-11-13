use std::collections::VecDeque;
use std::fmt;

use crate::model::Operation;

#[derive(Default)]
pub struct EditQueue {
    operations: VecDeque<(u64, Operation)>,
    acknowledged: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueError;

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("layout edits arrived out of order")
    }
}

impl std::error::Error for QueueError {}

impl EditQueue {
    pub fn acknowledged(&self) -> u64 {
        self.acknowledged
    }

    pub fn pending(&self) -> bool {
        !self.operations.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, &Operation)> {
        self.operations
            .iter()
            .map(|(sequence, operation)| (*sequence, operation))
    }

    pub fn push(&mut self, sequence: u64, operation: Operation) -> Result<bool, QueueError> {
        if sequence <= self.acknowledged {
            return Ok(false);
        }
        if self
            .operations
            .back()
            .is_some_and(|(last, _)| sequence <= *last)
        {
            return Err(QueueError);
        }
        self.operations.push_back((sequence, operation));
        Ok(true)
    }

    pub fn apply_prefix<E>(
        &mut self,
        mut apply: impl FnMut(&Operation) -> Result<(), E>,
    ) -> Result<(), E> {
        while let Some((sequence, operation)) = self.operations.front() {
            apply(operation)?;
            self.acknowledged = *sequence;
            self.operations.pop_front();
        }
        Ok(())
    }

    pub fn acknowledge(&mut self, through: u64) {
        self.acknowledged = self.acknowledged.max(through);
        while self
            .operations
            .front()
            .is_some_and(|(sequence, _)| *sequence <= self.acknowledged)
        {
            self.operations.pop_front();
        }
    }

    pub fn discard_after<E>(
        &mut self,
        through: u64,
        reload: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        reload()?;
        self.acknowledge(through);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Document, ModelError, Workspace};

    #[test]
    fn rejected_prefix_keeps_later_edits_and_retries_them_in_order_after_refresh() {
        let mut document = Document::default();
        let first = Workspace::new("first", "/first", None);
        let second = Workspace::new("second", "/second", None);
        let mut queue = EditQueue::default();
        queue
            .push(
                1,
                Operation::RenameWorkspace {
                    workspace: first.id.clone(),
                    name: "renamed".to_string(),
                },
            )
            .unwrap();
        queue
            .push(2, Operation::AddWorkspace(second.clone()))
            .unwrap();
        assert!(queue
            .apply_prefix(|operation| document.apply(operation).map(|_| ()))
            .is_err());
        assert_eq!(queue.acknowledged(), 0);
        assert!(document.workspace(&second.id).is_none());
        assert_eq!(queue.iter().count(), 2);
        document
            .apply(&Operation::AddWorkspace(first.clone()))
            .unwrap();
        queue
            .apply_prefix(|operation| document.apply(operation).map(|_| ()))
            .unwrap();
        assert_eq!(queue.acknowledged(), 2);
        assert!(!queue.pending());
        assert_eq!(document.workspace(&first.id).unwrap().name, "renamed");
        assert!(document.workspace(&second.id).is_some());
    }

    #[test]
    fn failed_discard_retains_edits_and_success_discards_only_through_its_barrier() {
        let mut queue = EditQueue::default();
        let workspaces: Vec<_> = ["first", "second", "newer"]
            .into_iter()
            .map(|name| Workspace::new(name, "/repo", None))
            .collect();
        for (index, workspace) in workspaces.iter().enumerate() {
            queue
                .push(index as u64 + 1, Operation::AddWorkspace(workspace.clone()))
                .unwrap();
        }
        let failed: Result<(), ModelError> =
            queue.discard_after(2, || Err(ModelError("reload failed".to_string())));
        assert!(failed.is_err());
        assert_eq!(queue.acknowledged(), 0);
        assert_eq!(queue.iter().count(), 3);
        queue.discard_after(2, || Ok::<_, ModelError>(())).unwrap();
        assert_eq!(queue.acknowledged(), 2);
        assert_eq!(
            queue
                .iter()
                .map(|(sequence, _)| sequence)
                .collect::<Vec<_>>(),
            vec![3]
        );
        let mut document = Document::default();
        queue
            .apply_prefix(|operation| document.apply(operation).map(|_| ()))
            .unwrap();
        assert_eq!(document.workspaces.len(), 1);
        assert_eq!(document.workspaces[0].id, workspaces[2].id);
        assert_eq!(queue.acknowledged(), 3);
    }

    #[test]
    fn acknowledged_updates_and_repeated_delivery_cannot_reapply_discarded_edits() {
        let mut queue = EditQueue::default();
        let operation = Operation::AddWorkspace(Workspace::new("old", "/repo", None));
        queue.push(1, operation.clone()).unwrap();
        queue.acknowledge(1);
        queue.acknowledge(0);
        assert!(!queue.push(1, operation).unwrap());
        assert!(!queue.pending());
        assert_eq!(queue.acknowledged(), 1);
    }
}
