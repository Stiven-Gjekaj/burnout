//! An NTFS volume, which Windows To Go starts Windows from.
//!
//! No crate writes NTFS under the MIT licence, so this does, from the public
//! descriptions of the format. It is P11 of [the roadmap], and it is not
//! complete: nothing outside this module calls it yet, so the parts that no
//! caller uses yet are allowed to be unused.
//!
//! [the roadmap]: https://github.com/Stiven-Gjekaj/burnout/blob/main/docs/roadmap.md

#![allow(dead_code)]

mod attrdef;
mod boot;
mod index;
mod record;
mod runs;
mod values;
