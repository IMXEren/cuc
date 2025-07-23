use kdl::KdlNode;
use std::{
    collections::{HashMap, HashSet},
    io,
};

use crate::namespace::NameSpace;

#[derive(Debug, Default, Clone)]
pub struct UsageSpec {
    pub info: Info,
    pub flags: Vec<Flag>,
    pub args: Vec<Arg>,
    pub cmds: Vec<Cmd>,
    pub completes: HashMap<String, Complete>,
}

#[derive(Debug, Default, Clone)]
pub struct Info {
    pub name: String,
    pub bin: String,
}

#[derive(Debug, Clone)]
pub enum Usage {
    Flag(Flag),
    Arg(Arg),
    Cmd(Cmd),
    Complete(Complete),
}

#[derive(Debug, Default, Clone)]
pub struct Alias {
    pub name: String,
    pub hide: bool,
}

#[derive(Debug, Default, Clone)]
pub enum GlobalFlag {
    #[default]
    None,
    Itself,
    Imposed(NameSpace),
}

#[derive(Debug, Default, Clone)]
pub struct Flag {
    pub name: String,
    pub names: Vec<String>,
    pub help: String,
    pub hide: bool,
    pub global: GlobalFlag,
    pub aliases: Vec<Alias>,
    pub arg: Option<Arg>,
}

#[derive(Debug, Default, Clone)]
pub struct Arg {
    pub name: String,
    pub repr: String,
    pub required: bool,
    pub choices: Vec<String>,
    pub hide: bool,
    pub var: bool,
    pub min: Option<i128>,
    pub max: Option<i128>,
    pub default: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Cmd {
    pub name: String,
    pub help: String,
    pub hide: bool,
    pub args: Vec<Arg>,
    pub flags: Vec<Flag>,
    pub aliases: Vec<Alias>,
    pub cmds: Vec<Box<Cmd>>,
}

#[derive(Debug, Default, Clone)]
pub struct Complete {
    pub name: String,
    pub kind: CompleteKind,
    pub descs: bool,
}

#[derive(Debug, Clone)]
pub enum CompleteKind {
    None,
    File,
    Dir,
    Run(String),
}

pub fn parse_name(node: &KdlNode) -> Result<String, UError> {
    if node.name().value() != "name" {
        return Err(UError::InvalidNodeName(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Node name wasn't name!\n{:?}", node),
        )));
    }
    let name = node
        .get(0)
        .map(|v| v.as_string().unwrap_or_default().to_string())
        .ok_or_else(|| {
            UError::InvalidNodeFirstArg(io::Error::new(
                io::ErrorKind::NotFound,
                format!("No name found in {:?}", node),
            ))
        })?;
    Ok(name)
}

pub fn parse_bin(node: &KdlNode) -> Result<String, UError> {
    if node.name().value() != "bin" {
        return Err(UError::InvalidNodeName(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Node name wasn't bin!\n{:?}", node),
        )));
    }
    let bin = node
        .get(0)
        .map(|v| v.as_string().unwrap_or_default().to_string())
        .ok_or_else(|| {
            UError::InvalidNodeFirstArg(io::Error::new(
                io::ErrorKind::NotFound,
                format!("No bin found in {:?}", node),
            ))
        })?;
    Ok(bin)
}

pub fn parse_include(node: &KdlNode) -> Result<String, UError> {
    if node.name().value() != "include" {
        return Err(UError::InvalidNodeName(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Node name wasn't include!\n{:?}", node),
        )));
    }
    let include = node
        .get(0)
        .map(|v| v.as_string().unwrap_or_default().to_string())
        .ok_or_else(|| {
            UError::InvalidNodeFirstArg(io::Error::new(
                io::ErrorKind::NotFound,
                format!("No include found in {:?}", node),
            ))
        })?;
    Ok(include)
}

pub fn parse_alias(node: &KdlNode) -> Result<Vec<Alias>, UError> {
    if node.name().value() != "alias" {
        return Err(UError::InvalidNodeName(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Node name wasn't alias!\n{:?}", node),
        )));
    }

    let mut aliases: Vec<Alias> = vec![];
    for entry in node.entries() {
        let mut hide = false;
        if entry.name().is_none() {
            let alias_name = entry
                .value()
                .as_string()
                .ok_or_else(|| {
                    UError::InvalidNodeFirstArg(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("No alias found in {:?}", entry),
                    ))
                })?
                .to_string();
            if let Some(hide_val) = node.get("hide") {
                hide = hide_val.as_bool().unwrap_or_default();
            }
            let alias = Alias {
                name: alias_name,
                hide,
            };
            aliases.push(alias);
        }
    }
    Ok(aliases)
}

pub fn parse_choices(node: &KdlNode) -> Result<Vec<String>, UError> {
    if node.name().value() != "choices" {
        return Err(UError::InvalidNodeName(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Node name wasn't choices!\n{:?}", node),
        )));
    }

    let mut choices: Vec<String> = vec![];
    for entry in node.entries() {
        let choice = entry
            .value()
            .as_string()
            .ok_or_else(|| {
                UError::InvalidNodeFirstArg(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("No choice found in {:?}", entry),
                ))
            })?
            .to_string();
        choices.push(choice);
    }
    Ok(choices)
}

pub fn parse_flag(node: &KdlNode) -> Result<Flag, UError> {
    if node.name().value() != "flag" {
        return Err(UError::InvalidNodeName(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Node name wasn't flag!\n{:?}", node),
        )));
    }

    let mut flag = Flag::default();
    for (index, entry) in node.entries().iter().enumerate() {
        if index == 0 {
            let entry_flag_names = entry
                .value()
                .as_string()
                .ok_or_else(|| {
                    UError::InvalidNodeFirstArg(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("No flag found in {:?}", entry),
                    ))
                })?
                .to_string();

            // the longest flag name is set as an identifier to flag.name
            let (long_flag_index, flag_names) = {
                let mut name_len = 0;
                let mut flag_index = 0;
                let mut long_flag_index = flag_index;
                let flag_names: Vec<String> = entry_flag_names
                    .split_whitespace()
                    .map(|s| {
                        let s = String::from(s);
                        let len = s.len();
                        if len > name_len {
                            name_len = len;
                            long_flag_index = flag_index;
                        };
                        flag_index += 1;
                        s
                    })
                    .collect();
                (long_flag_index, flag_names)
            };

            let slugify = |mut c: char| {
                if !c.is_alphanumeric() && c != '_' {
                    c = '_';
                }
                c
            };

            let flag_name = flag_names[long_flag_index]
                .trim_matches('-')
                .chars()
                .map(slugify)
                .collect();

            flag.name = flag_name;
            flag.names = flag_names;
        }

        if let Some(iden_name) = entry.name() {
            match iden_name.value() {
                "help" => flag.help = entry.value().as_string().unwrap_or_default().to_string(),
                "hide" => flag.hide = entry.value().as_bool().unwrap_or_default(),
                "global" => flag.global = entry.value().as_bool().unwrap_or_default().into(),
                "negate" => {
                    let negate_flag = entry.value().as_string().unwrap_or_default().to_string();
                    if !negate_flag.is_empty() {
                        flag.names.push(negate_flag);
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(child_doc) = node.children() {
        for child_node in child_doc.nodes() {
            match child_node.name().value() {
                "arg" => flag.arg = Some(parse_arg(child_node)?),
                "alias" => flag.aliases = parse_alias(child_node)?,
                "choices" => {
                    if let Some(arg_name) = flag.names.pop() {
                        let mut arg = Arg::default();
                        arg.name = arg_name;
                        arg.choices = parse_choices(child_node)?;
                        if arg.name.starts_with("<") {
                            arg.required = true;
                        }
                        flag.arg = Some(arg);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(flag)
}

pub fn parse_arg(node: &KdlNode) -> Result<Arg, UError> {
    if node.name().value() != "arg" {
        return Err(UError::InvalidNodeName(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Node name wasn't arg!\n{:?}", node),
        )));
    }

    let mut arg = Arg::default();
    for (index, entry) in node.entries().iter().enumerate() {
        if index == 0 {
            let entry_arg_name = entry
                .value()
                .as_string()
                .ok_or_else(|| {
                    UError::InvalidNodeFirstArg(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("No arg found in {:?}", entry),
                    ))
                })?
                .to_string();

            if entry_arg_name.starts_with("<") {
                arg.required = true;
                let end = entry_arg_name.find(">").unwrap_or(entry_arg_name.len());
                arg.name = entry_arg_name[1..end].to_string();
            } else if entry_arg_name.starts_with("[") {
                arg.required = false;
                let end = entry_arg_name.find("]").unwrap_or(entry_arg_name.len());
                arg.name = entry_arg_name[1..end].to_string();
            }
            arg.repr = entry_arg_name;
        }

        if let Some(iden_name) = entry.name() {
            match iden_name.value() {
                "hide" => arg.hide = entry.value().as_bool().unwrap_or_default(),
                "default" => arg.default = entry.value().as_string().map(String::from),
                "var" => arg.var = entry.value().as_bool().unwrap_or_default(),
                "var_max" => arg.max = entry.value().as_integer(),
                "var_min" => arg.min = entry.value().as_integer(),
                _ => {}
            }
        }
    }

    arg.max = arg.max.or(Some(-1));
    arg.min = arg.min.or(Some(0));

    if let Some(child_doc) = node.children() {
        for child_node in child_doc.nodes() {
            if child_node.name().value() == "choices" {
                let mut choices: Vec<String> = vec![];
                for cn_entry in child_node.entries() {
                    let choice = cn_entry
                        .value()
                        .as_string()
                        .expect(format!("No choice found in {:?}", cn_entry).as_str())
                        .to_string();
                    choices.push(choice);
                }
                arg.choices = choices;
            }
        }
    }
    Ok(arg)
}

pub fn parse_cmd(node: &KdlNode) -> Result<Cmd, UError> {
    if node.name().value() != "cmd" {
        return Err(UError::InvalidNodeName(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Node name wasn't cmd!\n{:?}", node),
        )));
    }

    let mut cmd = Cmd::default();
    for (index, entry) in node.entries().iter().enumerate() {
        if index == 0 {
            let entry_cmd_name = entry
                .value()
                .as_string()
                .ok_or_else(|| {
                    UError::InvalidNodeFirstArg(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("No cmd found in {:?}", entry),
                    ))
                })?
                .to_string();

            cmd.name = entry_cmd_name;
        }

        if let Some(iden_name) = entry.name() {
            match iden_name.value() {
                "help" => {
                    cmd.help = entry
                        .value()
                        .as_string()
                        .map(String::from)
                        .unwrap_or_default()
                }
                "hide" => cmd.hide = entry.value().as_bool().unwrap_or_default(),
                _ => {}
            }
        }
    }

    if let Some(child_doc) = node.children() {
        for child_node in child_doc.nodes() {
            match child_node.name().value() {
                "alias" => {
                    let mut alias = parse_alias(child_node)?;
                    cmd.aliases.append(&mut alias);
                }
                "flag" => {
                    let flag = parse_flag(child_node)?;
                    cmd.flags.push(flag);
                }
                "arg" => {
                    let arg = parse_arg(child_node)?;
                    cmd.args.push(arg);
                }
                "cmd" => {
                    let child_cmd = parse_cmd(child_node)?;
                    cmd.cmds.push(Box::new(child_cmd));
                }
                _ => {}
            }
        }
    }
    Ok(cmd)
}

pub fn parse_complete(node: &KdlNode) -> Result<Complete, UError> {
    if node.name().value() != "complete" {
        return Err(UError::InvalidNodeName(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Node name wasn't complete!\n{:?}", node),
        )));
    }

    let mut complete = Complete::default();
    for (index, entry) in node.entries().iter().enumerate() {
        if index == 0 {
            let entry_complete_name = entry
                .value()
                .as_string()
                .ok_or_else(|| {
                    UError::InvalidNodeFirstArg(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("No complete found in {:?}", entry),
                    ))
                })?
                .to_string();
            complete.name = entry_complete_name;
        }

        if let Some(iden_name) = entry.name() {
            match iden_name.value() {
                "descriptions" => complete.descs = entry.value().as_bool().unwrap_or_default(),
                "run" => {
                    let run = entry
                        .value()
                        .as_string()
                        .map(String::from)
                        .unwrap_or_default();
                    complete.kind = CompleteKind::Run(run);
                }
                "type" => {
                    let arg_type = entry.value().as_string().unwrap_or_default();
                    match arg_type {
                        "file" => complete.kind = CompleteKind::File,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    Ok(complete)
}

pub fn parse_usage(node: &KdlNode) -> Result<Option<Usage>, UError> {
    match node.name().value() {
        "flag" => Ok(Some(Usage::Flag(parse_flag(node)?))),
        "arg" => Ok(Some(Usage::Arg(parse_arg(node)?))),
        "cmd" => Ok(Some(Usage::Cmd(parse_cmd(node)?))),
        "complete" => {
            let complete = parse_complete(node)?;
            if !complete.kind.is_none() {
                Ok(Some(Usage::Complete(complete)))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

impl Complete {
    pub fn file_complete() -> Self {
        Self {
            name: "file".to_string(),
            kind: CompleteKind::File,
            descs: false,
        }
    }

    pub fn dir_complete() -> Self {
        Self {
            name: "file".to_string(),
            kind: CompleteKind::Dir,
            descs: false,
        }
    }
}

impl PartialEq for Flag {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.names.len() == other.names.len() && {
            let a: HashSet<_> = self.names.iter().collect();
            let b: HashSet<_> = other.names.iter().collect();
            a == b
        }
    }
}
impl Eq for Flag {}

impl PartialEq for Arg {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}
impl Eq for Arg {}

impl PartialEq for Cmd {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}
impl Eq for Cmd {}

impl PartialEq for Complete {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}
impl Eq for Complete {}

impl CompleteKind {
    pub fn is_none(&self) -> bool {
        match self {
            Self::None => true,
            _ => false,
        }
    }

    pub fn is_file(&self) -> bool {
        match self {
            Self::File => true,
            _ => false,
        }
    }

    pub fn run(&self) -> Option<&String> {
        match self {
            Self::Run(run) => Some(run),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum UError {
    InvalidNodeName(io::Error),
    InvalidNodeFirstArg(io::Error),
}

impl std::fmt::Display for UError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UError::InvalidNodeName(error) => error.fmt(f),
            UError::InvalidNodeFirstArg(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for UError {}

impl From<UError> for io::Error {
    fn from(value: UError) -> Self {
        match value {
            UError::InvalidNodeName(error) => error,
            UError::InvalidNodeFirstArg(error) => error,
        }
    }
}

impl AsRef<Cmd> for Cmd {
    fn as_ref(&self) -> &Cmd {
        self
    }
}

impl Default for CompleteKind {
    fn default() -> Self {
        Self::None
    }
}

impl From<bool> for GlobalFlag {
    fn from(value: bool) -> Self {
        match value {
            true => Self::Itself,
            false => Self::None,
        }
    }
}

impl Flag {
    pub fn is_global(&self) -> bool {
        match self.global {
            GlobalFlag::None => false,
            _ => true,
        }
    }

    pub fn is_global_itself(&self) -> bool {
        match self.global {
            GlobalFlag::Itself => true,
            _ => false,
        }
    }

    pub fn is_global_imposed(&self) -> bool {
        match self.global {
            GlobalFlag::Imposed(_) => true,
            _ => false,
        }
    }
}
