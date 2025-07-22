use kdl::KdlDocument;
use std::{
    borrow::BorrowMut,
    collections::HashMap,
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
};

use cuc::{
    namespace::NameSpace,
    usage::{parse_bin, parse_include, parse_name, parse_usage},
};

pub trait UsageSpecExt
where
    Self: Sized,
{
    fn load(file: Option<&PathBuf>) -> anyhow::Result<Self>;
    fn parse<S>(ctx: ParsingContext, source: S) -> anyhow::Result<Self>
    where
        S: AsRef<str>;
    fn merge(self, other: Self) -> Self;
    fn add_default_completes(completes: &mut HashMap<String, cuc::usage::Complete>);
    fn add_global_flag_to_cmd(flag: &cuc::usage::Flag, cmd: &mut cuc::usage::Cmd, nm: NameSpace);
    fn add_global_flags_to_all_subcmds<C>(cmds: &mut [C], nm: NameSpace)
    where
        C: BorrowMut<cuc::usage::Cmd>;
}

pub struct ParsingContext {
    source: ParsingSource,
}

enum ParsingSource {
    Stdin,
    File(PathBuf),
}

impl UsageSpecExt for cuc::usage::UsageSpec {
    fn load(file: Option<&PathBuf>) -> anyhow::Result<Self> {
        let (ctx, source) = if let Some(usage_kdl_path) = file {
            let ctx = ParsingContext {
                source: ParsingSource::File(usage_kdl_path.clone()),
            };
            (ctx, std::fs::read_to_string(usage_kdl_path)?)
        } else {
            let mut input = std::io::stdin();
            if !input.is_terminal() {
                let ctx = ParsingContext {
                    source: ParsingSource::Stdin,
                };
                let mut buf = String::new();
                input.read_to_string(&mut buf)?;
                (ctx, buf)
            } else {
                anyhow::bail!("stdin is not atty! No input provided");
            }
        };

        Self::parse(ctx, source)
    }

    fn parse<S>(ctx: ParsingContext, source: S) -> anyhow::Result<Self>
    where
        S: AsRef<str>,
    {
        let mut info = cuc::usage::Info::default();
        let mut flags: Vec<cuc::usage::Flag> = vec![];
        let mut args: Vec<cuc::usage::Arg> = vec![];
        let mut cmds: Vec<cuc::usage::Cmd> = vec![];
        let mut completes: HashMap<String, cuc::usage::Complete> = HashMap::new();
        let mut chspec: Option<Self> = None;

        let kdl_doc: KdlDocument = source.as_ref().parse()?;
        for node in kdl_doc.nodes() {
            match node.name().value() {
                "name" => info.name = parse_name(node)?,
                "bin" => info.bin = parse_bin(node)?,
                "include" => {
                    let include_path = parse_include(node)?;
                    let include_path = Path::new(&include_path);
                    let file = match include_path.is_relative() {
                        true => {
                            let parent = match ctx.source {
                                ParsingSource::Stdin => std::env::current_dir()?,
                                ParsingSource::File(ref path_buf) => {
                                    path_buf.parent().unwrap().to_path_buf()
                                }
                            };
                            let file = parent.join(include_path);
                            file
                        }
                        false => include_path.to_path_buf(),
                    };
                    chspec = Some(Self::load(Some(&file))?);
                }
                _ => {}
            }

            let usage = parse_usage(node)?;
            if let Some(usage) = usage {
                match usage {
                    cuc::usage::Usage::Flag(flag) if !flag.hide => flags.push(flag),
                    cuc::usage::Usage::Arg(arg) if !arg.hide => args.push(arg),
                    cuc::usage::Usage::Cmd(cmd) if !cmd.hide => cmds.push(cmd),
                    cuc::usage::Usage::Complete(complete) => {
                        completes.insert(complete.name.to_lowercase(), complete);
                    }
                    _ => (),
                };
            }
        }

        // Adding imposed global flags to its subsequent subcmd, recursively
        {
            let nm = NameSpace::root();
            Self::add_global_flags_to_all_subcmds(&mut cmds, nm.clone());

            for flag in &flags {
                if !flag.is_global_itself() {
                    continue;
                }
                for cmd in &mut cmds {
                    Self::add_global_flag_to_cmd(flag, cmd, nm.clone());
                }
            }
        }

        let mut usage_spec = cuc::usage::UsageSpec {
            info,
            flags,
            args,
            cmds,
            completes,
        };
        if let Some(spec) = chspec {
            usage_spec = usage_spec.merge(spec);
        }
        Ok(usage_spec)
    }

    /// Merges other into self, by overriding values present in self from other
    fn merge(mut self, other: Self) -> Self {
        if !other.info.name.is_empty() {
            self.info.name = other.info.name;
        }
        if !other.info.bin.is_empty() {
            self.info.bin = other.info.bin;
        }

        for oflag in other.flags {
            if let Some(index) = self.flags.iter().position(|f| f == &oflag) {
                self.flags.remove(index);
                self.flags.push(oflag);
            }
        }

        for oarg in other.args {
            if let Some(index) = self.args.iter().position(|a| a == &oarg) {
                self.args.remove(index);
                self.args.push(oarg);
            }
        }

        for ocmd in other.cmds {
            if let Some(index) = self.cmds.iter().position(|c| c == &ocmd) {
                self.cmds.remove(index);
                self.cmds.push(ocmd);
            }
        }

        for (func_name, complete) in other.completes {
            self.completes.insert(func_name, complete);
        }

        self
    }

    fn add_default_completes(completes: &mut HashMap<String, cuc::usage::Complete>) {
        completes.insert("file".into(), cuc::usage::Complete::file_complete());
        completes.insert("dir".into(), cuc::usage::Complete::dir_complete());
    }

    fn add_global_flag_to_cmd(flag: &cuc::usage::Flag, cmd: &mut cuc::usage::Cmd, nm: NameSpace) {
        if flag.is_global_itself() {
            let mut flag = flag.clone();
            if !cmd.flags.contains(&flag) {
                let global = cuc::usage::GlobalFlag::Imposed(nm);
                flag.global = global;
                cmd.flags.push(flag);
            }
        }
    }

    fn add_global_flags_to_all_subcmds<C>(cmds: &mut [C], nm: NameSpace)
    where
        C: BorrowMut<cuc::usage::Cmd>,
    {
        for cmd in cmds {
            let cmd: &mut cuc::usage::Cmd = cmd.borrow_mut();
            let chnm = nm.clone().join(&cmd.name);
            Self::add_global_flags_to_all_subcmds(&mut cmd.cmds, chnm.clone());

            for flag in &cmd.flags {
                if !flag.is_global_itself() {
                    continue;
                }

                for subcmd in &mut cmd.cmds {
                    Self::add_global_flag_to_cmd(flag, subcmd, chnm.clone());
                }
            }
        }
    }
}
