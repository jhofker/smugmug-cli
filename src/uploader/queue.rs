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

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for UploadQueue FIFO behavior and basic operations

    #[test]
    fn test_new_queue_is_empty() {
        let mut queue = UploadQueue::new();
        assert!(queue.next().is_none());
    }

    #[test]
    fn test_default_queue_is_empty() {
        let mut queue = UploadQueue::default();
        assert!(queue.next().is_none());
    }

    #[test]
    fn test_add_and_next() {
        let mut queue = UploadQueue::new();
        let path1 = PathBuf::from("/path/to/file1.jpg");
        let path2 = PathBuf::from("/path/to/file2.jpg");

        queue.add(path1.clone());
        queue.add(path2.clone());

        assert_eq!(queue.next(), Some(path1));
        assert_eq!(queue.next(), Some(path2));
        assert_eq!(queue.next(), None);
    }

    #[test]
    fn test_fifo_order() {
        let mut queue = UploadQueue::new();
        let paths: Vec<PathBuf> = (0..5)
            .map(|i| PathBuf::from(format!("/path/file{}.jpg", i)))
            .collect();

        for path in &paths {
            queue.add(path.clone());
        }

        for path in &paths {
            assert_eq!(queue.next().as_ref(), Some(path));
        }

        assert_eq!(queue.next(), None);
    }

    #[test]
    fn test_single_item() {
        let mut queue = UploadQueue::new();
        let path = PathBuf::from("/path/to/single.jpg");

        queue.add(path.clone());
        assert_eq!(queue.next(), Some(path));
        assert_eq!(queue.next(), None);
    }

    #[test]
    fn test_multiple_add_and_next_cycles() {
        let mut queue = UploadQueue::new();

        let path1 = PathBuf::from("/path/file1.jpg");
        queue.add(path1.clone());
        assert_eq!(queue.next(), Some(path1));

        let path2 = PathBuf::from("/path/file2.jpg");
        queue.add(path2.clone());
        assert_eq!(queue.next(), Some(path2));

        assert_eq!(queue.next(), None);
    }

    #[test]
    fn test_empty_path() {
        let mut queue = UploadQueue::new();
        let empty_path = PathBuf::from("");

        queue.add(empty_path.clone());
        assert_eq!(queue.next(), Some(empty_path));
    }
}
