//! Sidebar documents. In-memory for now; persistence is an open question (PLAN.md §7).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: String,
    pub title: String,
    pub markdown: String,
}

#[derive(Debug, Default)]
pub struct DocumentStore {
    docs: Vec<Document>,
    next_id: u64,
}

impl DocumentStore {
    pub fn create(&mut self, title: impl Into<String>, markdown: impl Into<String>) -> &Document {
        self.next_id += 1;
        self.docs.push(Document {
            id: format!("doc-{}", self.next_id),
            title: title.into(),
            markdown: markdown.into(),
        });
        self.docs.last().unwrap()
    }

    pub fn append(&mut self, id: &str, markdown: &str) -> Option<&Document> {
        let doc = self.docs.iter_mut().find(|d| d.id == id)?;
        doc.markdown.push_str(markdown);
        Some(doc)
    }

    pub fn replace(&mut self, id: &str, markdown: impl Into<String>) -> Option<&Document> {
        let doc = self.docs.iter_mut().find(|d| d.id == id)?;
        doc.markdown = markdown.into();
        Some(doc)
    }

    pub fn get(&self, id: &str) -> Option<&Document> {
        self.docs.iter().find(|d| d.id == id)
    }

    /// Oldest first.
    pub fn ids(&self) -> Vec<String> {
        self.docs.iter().map(|d| d.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_append_replace() {
        let mut store = DocumentStore::default();
        let id = store.create("Title", "a").id.clone();
        assert_eq!(store.append(&id, "b").unwrap().markdown, "ab");
        assert_eq!(store.replace(&id, "c").unwrap().markdown, "c");
        assert_eq!(store.ids(), vec![id]);
        assert!(store.get("missing").is_none());
    }
}
