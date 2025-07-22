pub trait StringExt {
    fn trim_end_matches_mut<P>(&mut self, pat: P)
    where
        P: AsRef<str>;
}

impl StringExt for String {
    fn trim_end_matches_mut<P>(&mut self, pat: P)
    where
        P: AsRef<str>,
    {
        let pat = pat.as_ref();
        let pat_len = pat.len();
        while self.ends_with(pat) {
            let last_len = self.len().saturating_sub(pat_len);
            self.truncate(last_len);
        }
    }
}
