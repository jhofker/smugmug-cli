use std::collections::VecDeque;
use std::path::PathBuf;

pub struct UploadQueue {
    queue: VecDeque<PathBuf>,
}

impl UploadQueue {
    pub fn new() -> Self {
        UploadQueue {
            queue: VecDeque::new(),
        }
    }

    pub fn add(&mut self, path: PathBuf) {
        self.queue.push_back(path);
    }

    pub fn next(&mut self) -> Option<PathBuf> {
        self.queue.pop_front()
    }
}

impl Default for UploadQueue {
    fn default() -> Self {
        Self::new()
    }
}
