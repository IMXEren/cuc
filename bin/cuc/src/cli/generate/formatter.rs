use std::fmt;

use cuc::namespace::NameSpace;

#[derive(Default, Debug, Clone)]
pub struct GenFormatter {
    pub ns: NameSpace,
    pub level: usize,
}

impl GenFormatter {
    const TAB4SPACES: &'static str = "\t";

    pub fn increment_level(&mut self) {
        self.level += 1;
    }

    pub fn decrement_level(&mut self) {
        if self.level > 0 {
            self.level -= 1;
        }
    }

    pub fn indent<W>(&self, buf: &mut W)
    where
        W: fmt::Write,
    {
        for _ in 0..self.level + 1 {
            write!(buf, "{}", Self::TAB4SPACES).unwrap();
        }
    }

    pub fn newline<W>(&self, buf: &mut W)
    where
        W: fmt::Write,
    {
        write!(buf, "\n").unwrap();
    }
}
