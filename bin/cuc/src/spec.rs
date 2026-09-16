use std::{
    collections::HashMap,
    io::{IsTerminal, Read},
    path::PathBuf,
};

use cuc::{
    namespace::NameSpace,
    usage::{
        Alias, Arg, Cmd, Complete, CompleteKind, DoubleDash, Flag, GlobalFlag, Info, PendingMount,
    },
};
use usage::{Spec, SpecArg, SpecClause, SpecCommand, SpecComplete, SpecFlag};

pub trait UsageSpecExt
where
    Self: Sized,
{
    fn load(file: Option<&PathBuf>) -> anyhow::Result<Self>;
    fn add_default_completes(completes: &mut HashMap<String, Complete>);
}

impl UsageSpecExt for cuc::usage::UsageSpec {
    fn load(file: Option<&PathBuf>) -> anyhow::Result<Self> {
        let spec = if let Some(path) = file {
            Spec::parse_file(path)?
        } else {
            let mut input = std::io::stdin();
            if input.is_terminal() {
                anyhow::bail!("stdin is a TTY and no input was provided");
            }
            let mut source = String::new();
            input.read_to_string(&mut source)?;
            source.parse::<Spec>()?
        };

        let mut pending = PendingMounts::default();
        collect_pending_mounts(&spec.cmd, &NameSpace::root(), &mut pending);
        Ok(convert_spec(spec, &pending))
    }

    fn add_default_completes(completes: &mut HashMap<String, Complete>) {
        completes.insert("file".into(), Complete::file_complete());
        completes.insert("dir".into(), Complete::dir_complete());
    }
}

/// A mount's commands, keyed by the namespace of the command that declares it.
///
/// They are kept out of the spec because a mount is not resolved while generating:
/// usage runs the mount command, and so does the generated completion, at the moment
/// it is asked for a completion.
type PendingMounts = HashMap<String, Vec<PendingMount>>;

/// Record every mount as dynamic. Usage resolves a mount by running its command, which
/// is exactly what the generated completion does too: the mounted commands belong to the
/// directory the user is in, not to the one that generated the file, so recording the
/// output here would freeze a snapshot that is wrong everywhere but here.
fn collect_pending_mounts(cmd: &SpecCommand, namespace: &NameSpace, pending: &mut PendingMounts) {
    if !cmd.mounts.is_empty() {
        pending
            .entry(namespace.display())
            .or_default()
            .extend(cmd.mounts.iter().map(|mount| PendingMount {
                run: mount.run.clone(),
                synopsis: mount.synopsis.clone(),
                overrides_default: mount.overrides_default,
            }));
    }

    for subcommand in cmd.subcommands.values() {
        collect_pending_mounts(
            subcommand,
            &namespace
                .clone()
                .join(cuc::namespace::slugify(&subcommand.name)),
            pending,
        );
    }
}

fn convert_spec(spec: Spec, pending: &PendingMounts) -> cuc::usage::UsageSpec {
    let root_namespace = NameSpace::root();
    let mut flags = spec
        .cmd
        .flags
        .iter()
        .chain(spec.cmd.clause.iter().flat_map(|clause| &clause.flags))
        .filter(|flag| !flag.hide)
        .map(convert_flag)
        .collect::<Vec<_>>();
    if spec.default_subcommand_flags
        && let Some(default) = spec
            .default_subcommand
            .as_ref()
            .and_then(|name| spec.cmd.subcommands.get(name))
    {
        for mut flag in default
            .flags
            .iter()
            .chain(default.clause.iter().flat_map(|clause| &clause.flags))
            .filter(|flag| !flag.hide)
            .map(convert_flag)
        {
            if !flags.contains(&flag) {
                flag.link_to = Some(default.name.clone());
                flags.push(flag);
            }
        }
    }
    let inherited = flags
        .iter()
        .filter(|flag| flag.is_global_itself())
        .cloned()
        .map(|flag| (flag, root_namespace.clone()))
        .collect::<Vec<_>>();
    let root_sigils = spec
        .cmd
        .clause
        .as_ref()
        .map(|clause| &clause.args)
        .unwrap_or(&spec.cmd.args)
        .iter()
        .filter(|arg| !arg.hide && arg.sigil.is_some())
        .map(convert_arg)
        .collect::<Vec<_>>();
    let mut completes = convert_completes(&spec.complete, &root_namespace);
    if spec.default_subcommand_flags
        && let Some(default) = spec
            .default_subcommand
            .as_ref()
            .and_then(|name| spec.cmd.subcommands.get(name))
    {
        completes.extend(convert_completes(&default.complete, &root_namespace));
    }
    let mut args = match &spec.cmd.clause {
        Some(clause) => convert_clause_args(clause),
        None => spec.cmd.args.iter().map(convert_arg).collect(),
    };
    mark_double_dash_jump(&mut args);
    let cmds = spec
        .cmd
        .subcommands
        .values()
        .filter(|cmd| !cmd.hide)
        .map(|cmd| {
            convert_cmd(
                cmd,
                &root_namespace,
                &inherited,
                false,
                &root_sigils,
                pending,
            )
        })
        .collect();
    let mut root_mounts = pending
        .get(&root_namespace.display())
        .cloned()
        .unwrap_or_default();
    if spec.default_subcommand.is_some() && !root_mounts.iter().any(|mount| mount.overrides_default)
    {
        root_mounts.clear();
    }

    cuc::usage::UsageSpec {
        info: Info {
            name: spec.name,
            bin: spec.bin,
        },
        flags,
        args,
        cmds,
        completes,
        default_subcommand: spec.default_subcommand,
        default_subcommand_flags: spec.default_subcommand_flags,
        pending_mounts: root_mounts,
        sigils: root_sigils,
        restart_token: spec.cmd.restart_token,
        clause: spec.cmd.clause.as_ref().map(convert_clause),
    }
}

fn convert_cmd(
    command: &SpecCommand,
    parent_namespace: &NameSpace,
    inherited: &[(Flag, NameSpace)],
    parent_mounted: bool,
    inherited_sigils: &[Arg],
    pending: &PendingMounts,
) -> Cmd {
    let namespace = parent_namespace
        .clone()
        .join(cuc::namespace::slugify(&command.name));
    let inherited = if command.mounted && !parent_mounted {
        &[][..]
    } else {
        inherited
    };
    let mount_prefix_flags = inherited
        .iter()
        .map(|(flag, origin)| {
            let mut flag = flag.clone();
            flag.global = GlobalFlag::Imposed(origin.clone());
            flag
        })
        .collect::<Vec<_>>();
    // A subcommand inherits the sigils its ancestors declared, so the sigil keeps
    // classifying arguments at every depth.
    let mut sigils = inherited_sigils.to_vec();
    sigils.extend(
        command
            .clause
            .as_ref()
            .map(|clause| &clause.args)
            .unwrap_or(&command.args)
            .iter()
            .filter(|arg| !arg.hide && arg.sigil.is_some())
            .map(convert_arg),
    );
    let mut flags = command
        .flags
        .iter()
        // A clause's flags are ordinary flags of the command: usage gathers them from
        // the command and its clause alike when deciding what is available.
        .chain(command.clause.iter().flat_map(|clause| &clause.flags))
        .filter(|flag| !flag.hide)
        .map(convert_flag)
        .collect::<Vec<_>>();

    for (flag, origin) in inherited {
        if !flags.contains(flag) {
            let mut imposed = flag.clone();
            imposed.global = GlobalFlag::Imposed(origin.clone());
            flags.push(imposed);
        }
    }

    let mut child_inherited = inherited.to_vec();
    child_inherited.extend(
        flags
            .iter()
            .filter(|flag| flag.is_global_itself())
            .cloned()
            .map(|flag| (flag, namespace.clone())),
    );

    let mut args = match &command.clause {
        Some(clause) => convert_clause_args(clause),
        None => command.args.iter().map(convert_arg).collect(),
    };
    mark_double_dash_jump(&mut args);
    let pending_mounts = pending
        .get(&namespace.display())
        .cloned()
        .unwrap_or_default();

    Cmd {
        name: command.name.clone(),
        help: command.help.clone().unwrap_or_default(),
        hide: command.hide,
        // A clause's positionals are the command's positionals: usage parses an
        // active command's arguments from its clause whenever it has one.
        args,
        flags,
        aliases: command
            .aliases
            .iter()
            .map(|name| Alias {
                name: name.clone(),
                hide: false,
            })
            .chain(command.hidden_aliases.iter().map(|name| Alias {
                name: name.clone(),
                hide: true,
            }))
            .collect(),
        cmds: command
            .subcommands
            .values()
            .filter(|cmd| !cmd.hide)
            .map(|cmd| {
                Box::new(convert_cmd(
                    cmd,
                    &namespace,
                    &child_inherited,
                    command.mounted,
                    &sigils,
                    pending,
                ))
            })
            .collect(),
        completes: convert_completes(&command.complete, &namespace),
        mounted: command.mounted,
        pending_mounts,
        mount_prefix_flags,
        sigils,
        restart_token: command.restart_token.clone(),
        clause: command.clause.as_ref().map(convert_clause),
    }
}

fn convert_clause(clause: &SpecClause) -> cuc::usage::Clause {
    let mut args = clause.args.iter().map(convert_arg).collect::<Vec<_>>();
    mark_double_dash_jump(&mut args);
    cuc::usage::Clause {
        name: clause.name.clone(),
        separator: clause.separator.clone(),
        help: clause.help.clone().unwrap_or_default(),
        flags: clause
            .flags
            .iter()
            .filter(|flag| !flag.hide)
            .map(convert_flag)
            .collect(),
        sigils: args
            .iter()
            .filter(|arg| !arg.hide && arg.sigil.is_some())
            .cloned()
            .collect(),
        args,
    }
}

/// A clause is a repeatable group of scoped flags and positionals. Clink has no
/// separator or repetition concept, so a repeated single-positional clause becomes one
/// optional variadic argument, exactly as usage renders it, while a
/// multi-positional separator clause stays a single group.
fn convert_clause_args(clause: &SpecClause) -> Vec<Arg> {
    let mut args = clause.args.iter().map(convert_arg).collect::<Vec<_>>();
    if args.len() == 1 {
        let arg = &mut args[0];
        arg.required = false;
        arg.var = true;
        arg.min = Some(0);
        arg.max = Some(-1);
        arg.repr = clause.usage.clone();
    }
    args
}

fn convert_flag(flag: &SpecFlag) -> Flag {
    let mut names = flag
        .short
        .iter()
        .map(|name| format!("-{name}"))
        .chain(flag.long.iter().map(|name| format!("--{name}")))
        .collect::<Vec<_>>();
    if let Some(negate) = &flag.negate {
        names.push(negate.clone());
    }

    Flag {
        name: flag.name.clone(),
        names,
        help: flag.help.clone().unwrap_or_default(),
        hide: flag.hide,
        global: flag.global.into(),
        aliases: flag
            .hidden_short_aliases
            .iter()
            .map(|name| Alias {
                name: format!("-{name}"),
                hide: true,
            })
            .chain(flag.hidden_aliases.iter().map(|name| Alias {
                name: format!("--{name}"),
                hide: true,
            }))
            .collect(),
        arg: flag.arg.as_ref().map(convert_arg),
        link_to: None,
    }
}

fn mark_double_dash_jump(args: &mut [Arg]) {
    if let Some(required) = args
        .iter()
        .position(|arg| arg.double_dash == DoubleDash::Required)
    {
        for arg in &mut args[..required] {
            arg.skip_after_double_dash = true;
        }
    }
}

fn convert_arg(arg: &SpecArg) -> Arg {
    Arg {
        name: arg.name.clone(),
        repr: arg.usage.clone(),
        required: arg.required,
        choices: arg
            .choices
            .as_ref()
            .map(|choices| {
                choices
                    .choices
                    .iter()
                    .cloned()
                    .chain(
                        choices
                            .details
                            .iter()
                            .filter(|choice| !choice.hide)
                            .map(|choice| choice.value.clone()),
                    )
                    .collect()
            })
            .unwrap_or_default(),
        hide: arg.hide,
        var: arg.var,
        min: arg.var.then(|| arg.var_min.unwrap_or(0) as i128),
        max: arg
            .var
            .then(|| arg.var_max.map_or(-1, |value| value as i128)),
        default: arg.default.first().cloned(),
        sigil: arg.sigil.clone(),
        skip_after_double_dash: false,
        link_after: None,
        double_dash: match arg.double_dash {
            usage::SpecDoubleDashChoices::Automatic => DoubleDash::Automatic,
            usage::SpecDoubleDashChoices::Optional => DoubleDash::Optional,
            usage::SpecDoubleDashChoices::Required => DoubleDash::Required,
            usage::SpecDoubleDashChoices::Preserve => DoubleDash::Preserve,
        },
    }
}

fn convert_completes(
    completes: &indexmap::IndexMap<String, SpecComplete>,
    namespace: &NameSpace,
) -> HashMap<String, Complete> {
    completes
        .iter()
        .filter_map(|(name, complete)| {
            let kind = if let Some(run) = &complete.run {
                CompleteKind::Run(run.clone())
            } else {
                match complete.type_.as_deref() {
                    Some("file") => CompleteKind::File,
                    Some("dir") => CompleteKind::Dir,
                    _ => return None,
                }
            };
            let scoped_name = if namespace.is_root() {
                name.clone()
            } else {
                format!("{}::{name}", namespace.display())
            };
            Some((
                name.to_lowercase(),
                Complete {
                    // Completion functions are global Lua symbols. Include the command scope
                    // so equal argument names under different commands cannot overwrite one
                    // another in the generator cache.
                    name: scoped_name,
                    kind,
                    descs: complete.descriptions,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> cuc::usage::UsageSpec {
        convert_spec(source.parse::<Spec>().unwrap(), &PendingMounts::default())
    }

    #[test]
    fn parses_inline_flag_args_and_resolves_flagsets() {
        let spec = parse(
            r#"
                name "demo"
                bin "demo"
                flagset "common" {
                    flag "-v --verbose"
                }
                use "common"
                flag "-u --user <user>" {
                    choices "alice" "bob"
                }
            "#,
        );

        assert_eq!(spec.flags.len(), 2);
        assert_eq!(spec.flags[0].names, ["-v", "--verbose"]);
        let user = &spec.flags[1];
        assert_eq!(user.names, ["-u", "--user"]);
        let arg = user.arg.as_ref().unwrap();
        assert_eq!(arg.name, "user");
        assert_eq!(arg.repr, "<user>");
        assert_eq!(arg.choices, ["alice", "bob"]);
    }

    #[test]
    fn preserves_variadic_double_dash_and_default_subcommand_semantics() {
        let spec = parse(
            r#"
                name "demo"
                bin "demo"
                default_subcommand "run"
                default_subcommand_flags #true
                arg "[args]" var=#true var_min=1 double_dash="required"
                cmd "run" {
                    flag "--jobs <count>"
                    arg "[task]"
                }
            "#,
        );

        assert_eq!(spec.default_subcommand.as_deref(), Some("run"));
        assert!(spec.default_subcommand_flags);
        assert!(spec.flags.iter().any(|flag| flag.names == ["--jobs"]));
        assert!(spec.args[0].var);
        assert_eq!(spec.args[0].min, Some(1));
        assert_eq!(spec.args[0].max, Some(-1));
        assert_eq!(spec.args[0].double_dash, DoubleDash::Required);
    }

    #[test]
    fn converts_resolved_mounts() {
        let mut spec = r#"
            name "demo"
            bin "demo"
            mount run="discover"
        "#
        .parse::<Spec>()
        .unwrap();
        spec.resolve_mount_outputs(&HashMap::from([(
            "discover".to_string(),
            "cmd \"mounted\" { flag \"--from-mount\" }".to_string(),
        )]))
        .unwrap();

        let spec = convert_spec(spec, &PendingMounts::default());
        assert_eq!(spec.cmds[0].name, "mounted");
        assert!(spec.cmds[0].mounted);
        assert_eq!(spec.cmds[0].flags[0].names, ["--from-mount"]);
    }

    #[test]
    fn records_mounts_as_pending_without_running_them() {
        let spec = r#"
            name "demo"
            bin "demo"
            mount run="this-command-does-not-exist" synopsis="[TASK] [ARGS]…"
            cmd "run" {
                mount run="neither does this one" synopsis="[NAME]"
            }
        "#
        .parse::<Spec>()
        .unwrap();

        let mut pending = PendingMounts::default();
        collect_pending_mounts(&spec.cmd, &NameSpace::root(), &mut pending);
        let spec = convert_spec(spec, &pending);

        assert_eq!(
            spec.pending_mounts,
            [PendingMount {
                run: "this-command-does-not-exist".into(),
                synopsis: Some("[TASK] [ARGS]…".into()),
                overrides_default: false,
            }]
        );
        assert_eq!(
            spec.cmds[0].pending_mounts,
            [PendingMount {
                run: "neither does this one".into(),
                synopsis: Some("[NAME]".into()),
                overrides_default: false,
            }]
        );
        // Nothing is grafted: the mounted commands stay unknown until completion.
        assert!(spec.cmds.iter().all(|cmd| cmd.cmds.is_empty()));
    }

    #[test]
    fn root_mount_respects_default_subcommand_precedence() {
        let ordinary = r#"
            name "demo"
            bin "demo"
            default_subcommand "run"
            mount run="discover"
            cmd "run" { arg "<task>" }
        "#
        .parse::<Spec>()
        .unwrap();
        let mut pending = PendingMounts::default();
        collect_pending_mounts(&ordinary.cmd, &NameSpace::root(), &mut pending);
        assert!(convert_spec(ordinary, &pending).pending_mounts.is_empty());

        let overriding = r#"
            name "demo"
            bin "demo"
            default_subcommand "run"
            mount run="discover" overrides_default=#true
            cmd "run" { arg "<task>" }
        "#
        .parse::<Spec>()
        .unwrap();
        let mut pending = PendingMounts::default();
        collect_pending_mounts(&overriding.cmd, &NameSpace::root(), &mut pending);
        let converted = convert_spec(overriding, &pending);
        assert_eq!(converted.pending_mounts.len(), 1);
        assert!(converted.pending_mounts[0].overrides_default);
    }

    #[test]
    fn includes_clause_flags_and_makes_its_positional_variadic() {
        let spec = parse(
            r#"
                name "demo"
                bin "demo"
                cmd "use" {
                    flag "--frozen"
                    clause tools {
                        flag "--postinstall <COMMAND>"
                        flag "--tool-option <KEY=VALUE>" var=#true
                        arg "<TOOL@VERSION>"
                    }
                }
            "#,
        );

        let use_cmd = &spec.cmds[0];
        let names = use_cmd
            .flags
            .iter()
            .map(|flag| flag.names[0].as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["--frozen", "--postinstall", "--tool-option"]);

        // An implicit clause repeats its single positional, so it is optional and
        // variadic here, with the clause's own synopsis as its hint.
        assert_eq!(use_cmd.args.len(), 1);
        let arg = &use_cmd.args[0];
        assert_eq!(arg.name, "TOOL@VERSION");
        assert!(!arg.required);
        assert!(arg.var);
        assert_eq!(arg.min, Some(0));
        assert_eq!(arg.max, Some(-1));
        assert_eq!(arg.repr, "[TOOL@VERSION]…");
    }

    #[test]
    fn clause_positionals_become_the_command_arguments() {
        // usage rejects a command that declares both top-level arguments and a clause, so
        // a command's positionals come from whichever of the two it has.
        let spec = parse(
            r#"
                name "demo"
                bin "demo"
                cmd "a" {
                    clause "tools" {
                        arg "<TOOL>"
                    }
                }
                cmd "b" {
                    arg "[kept]"
                }
            "#,
        );

        let names = spec.cmds[0]
            .args
            .iter()
            .map(|arg| arg.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["TOOL"]);
        assert_eq!(spec.cmds[0].args[0].repr, "[TOOL]…");
        assert_eq!(spec.cmds[1].args[0].name, "kept");
        assert!(!spec.cmds[1].args[0].var);
    }

    #[test]
    fn keeps_multi_positional_separator_clauses_as_one_group() {
        let spec = parse(
            r#"
                name "demo"
                bin "demo"
                cmd "a" {
                    clause "pair" separator=":::" {
                        arg "[LEFT]"
                        arg "[RIGHT]"
                    }
                }
            "#,
        );

        let args = &spec.cmds[0].args;
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].name, "LEFT");
        assert_eq!(args[1].name, "RIGHT");
        // Clink cannot repeat a group around a separator, so neither arg absorbs more.
        assert!(args.iter().all(|arg| !arg.var));
    }

    #[test]
    fn sigil_arguments_are_recorded_and_inherited_by_subcommands() {
        let spec = parse(
            r#"
            name "demo"
            bin "demo"
            arg "[tools]..." sigil="+" {
                choices "node@22"
            }
            cmd "run" {
                arg "[task]"
            }
        "#,
        );

        assert_eq!(spec.sigils.len(), 1);
        assert_eq!(spec.sigils[0].sigil.as_deref(), Some("+"));
        assert_eq!(spec.sigils[0].choices, ["node@22"]);
        // A subcommand inherits the sigils its ancestors declared.
        assert_eq!(spec.cmds[0].sigils.len(), 1);
        assert_eq!(spec.cmds[0].sigils[0].sigil.as_deref(), Some("+"));
    }

    #[test]
    fn command_scoped_completers_have_distinct_function_identities() {
        let spec = parse(
            r#"
            name "demo"
            bin "demo"
            cmd "one" {
                arg "<target>"
                complete target run="echo one"
            }
            cmd "two" {
                arg "<target>"
                complete target run="echo two"
            }
        "#,
        );

        let one = spec.cmds[0].completes.get("target").unwrap();
        let two = spec.cmds[1].completes.get("target").unwrap();
        assert_eq!(one.name, "one::target");
        assert_eq!(two.name, "two::target");
        assert_ne!(one.name, two.name);
    }

    #[test]
    fn hidden_arguments_still_preserve_parser_boundaries() {
        let spec = parse(
            r#"
                name "demo"
                bin "demo"
                cmd "task" {
                    arg "[ARGS]…" hide=#true var=#true
                }
            "#,
        );

        assert_eq!(spec.cmds[0].args.len(), 1);
        assert!(spec.cmds[0].args[0].hide);
        assert!(spec.cmds[0].args[0].var);
    }

    #[test]
    fn restart_token_is_recorded_on_the_command() {
        let spec = parse(
            r#"
            name "demo"
            bin "demo"
            cmd "run" restart_token=":::" {
                arg "[task]"
            }
        "#,
        );

        assert_eq!(spec.cmds[0].restart_token.as_deref(), Some(":::"));
    }

    #[test]
    fn marks_static_parser_transitions() {
        let spec = r#"
            name "demo"
            bin "demo"
            default_subcommand "run"
            default_subcommand_flags #true
            cmd "run" { flag "--jobs <COUNT>" }
            cmd "last" {
                arg "[BEFORE]"
                arg "[-- AFTER]…"
            }
        "#
        .parse::<Spec>()
        .unwrap();
        let converted = convert_spec(spec, &PendingMounts::default());
        let default_flag = converted
            .flags
            .iter()
            .find(|flag| flag.name == "jobs")
            .expect("default command flag");
        assert_eq!(default_flag.link_to.as_deref(), Some("run"));
        let last = converted
            .cmds
            .iter()
            .find(|cmd| cmd.name == "last")
            .expect("last command");
        assert!(last.args[0].skip_after_double_dash);
        assert_eq!(last.args[1].double_dash, DoubleDash::Required);
    }

    #[test]
    fn records_nested_mounts_in_their_own_namespace() {
        let spec = r#"
            name "demo"
            bin "demo"
            cmd "a" {
                cmd "b" {
                    mount run="discover"
                }
            }
        "#
        .parse::<Spec>()
        .unwrap();

        let mut pending = PendingMounts::default();
        collect_pending_mounts(&spec.cmd, &NameSpace::root(), &mut pending);
        let spec = convert_spec(spec, &pending);

        assert!(spec.pending_mounts.is_empty());
        assert!(spec.cmds[0].pending_mounts.is_empty());
        assert_eq!(spec.cmds[0].cmds[0].pending_mounts[0].run, "discover");
    }
}
