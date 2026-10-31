-- The day a statement counts a leg, where that is not its transaction's date: a
-- card line stays on its purchase date, and its balance moves when it posts.
ALTER TABLE postings ADD COLUMN posted_date TEXT;
