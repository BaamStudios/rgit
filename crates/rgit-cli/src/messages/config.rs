// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git config` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git config list [<file-option>] [<display-option>] [--includes]",
    "git config get [<file-option>] [<display-option>] [--includes] [--all] [--regexp] [--value=<pattern>] [--fixed-value] [--default=<default>] [--url=<url>] <name>",
    "git config set [<file-option>] [--type=<type>] [--all] [--value=<pattern>] [--fixed-value] <name> <value>",
    "git config unset [<file-option>] [--all] [--value=<pattern>] [--fixed-value] <name>",
    "git config rename-section [<file-option>] <old-name> <new-name>",
    "git config remove-section [<file-option>] <name>",
    "git config edit [<file-option>]",
    "git config [<file-option>] --get-colorbool <name> [<stdout-is-tty>]",
];
pub const USAGE_LIST: &str = "git config list [<file-option>] [<display-option>] [--includes]";
pub const USAGE_GET: &str = "git config get [<file-option>] [<display-option>] [--includes] [--all] [--regexp=<regexp>] [--value=<pattern>] [--fixed-value] [--default=<default>] <name>";
pub const USAGE_SET: &str = "git config set [<file-option>] [--type=<type>] [--comment=<message>] [--all] [--value=<pattern>] [--fixed-value] <name> <value>";
pub const USAGE_UNSET: &str =
    "git config unset [<file-option>] [--all] [--value=<pattern>] [--fixed-value] <name>";
pub const USAGE_RENAME: &str = "git config rename-section [<file-option>] <old-name> <new-name>";
pub const USAGE_REMOVE: &str = "git config remove-section [<file-option>] <name>";
pub const USAGE_EDIT: &str = "git config edit [<file-option>]";

pub const NO_ACTION: &str = "no action specified";
pub const ONE_FILE: &str = "only one config file at a time";
pub const ONE_TYPE: &str = "only one type at a time";
pub const NOT_IN_GIT_DIR: &str = "not in a git directory";
pub const HOME_NOT_SET: &str = "$HOME not set";
pub const WRITE_STDIN: &str = "writing to stdin is not supported";
pub const EDIT_STDIN: &str = "editing stdin is not supported";
pub const NAME_ONLY: &str = "--name-only is only applicable to --list or --get-regexp";
pub const SHOW_ORIGIN: &str =
    "--show-origin is only applicable to --get, --get-all, --get-regexp, and --list";
pub const DEFAULT_ONLY_GET: &str = "--default is only applicable to --get";
pub const COMMENT_ONLY_WRITE: &str = "--comment is only applicable to add/set/replace operations";
pub const FIXED_VALUE_LEGACY: &str = "--fixed-value only applies with 'value-pattern'";
pub const FIXED_VALUE_NEW: &str = "--fixed-value only applies with --value=<pattern>";
pub const APPEND_VALUE: &str = "--append cannot be used with --value=<pattern>";
pub const DEFAULT_ALL: &str = "--default= cannot be used with --all or --url=";
pub const WORKTREE_MULTIPLE: &str = "--worktree cannot be used with multiple working trees unless the config\nextension worktreeConfig is enabled. Please read \"CONFIGURATION FILE\"\nsection in \"git help worktree\" for details";
pub const GLOBAL_TEMPLATE_HEAD: &str = "# This is Git's per-user configuration file.\n[user]\n# Please adapt and uncomment the following lines:\n";

pub fn wrong_args(min: usize, max: usize) -> String {
    if min == max {
        format!("wrong number of arguments, should be {min}")
    } else {
        format!("wrong number of arguments, should be from {min} to {max}")
    }
}
pub fn unrecognized_type(t: &str) -> String {
    format!("unrecognized --type argument, {t}")
}
pub fn inside_repo_only(opt: &str) -> String {
    format!("{opt} can only be used inside a git repository")
}
pub fn invalid_key_pattern(p: &str) -> String {
    format!("invalid key pattern: {p}")
}
pub fn unable_to_read(path: &str, err: &str) -> String {
    format!("unable to read config file '{path}': {err}")
}
pub fn unable_to_access(path: &str, err: &str) -> String {
    format!("unable to access '{path}': {err}")
}
pub fn missing_value(key: &str, valid: bool) -> String {
    if valid {
        format!("missing value to set to the variable '{key}'")
    } else {
        format!("missing value to set to a variable with an invalid name '{key}'")
    }
}
pub fn did_you_mean(key: &str, value: &str) -> String {
    format!("did you mean \"git config set {key} {value}\"?")
}
pub fn editor_problem(editor: &str) -> String {
    format!("there was a problem with the editor '{editor}'")
}
pub fn not_implemented(what: &str) -> String {
    format!("rgit: {what} is not implemented yet")
}

pub fn bad_numeric(value: &str, name: &str, reason: &str) -> String {
    format!("bad numeric config value '{value}' for '{name}': {reason}")
}
pub fn bad_boolean(value: &str, name: &str) -> String {
    format!("bad boolean config value '{value}' for '{name}'")
}
pub fn expand_user_dir(path: &str) -> String {
    format!("failed to expand user dir in: '{path}'")
}
pub fn invalid_timestamp(value: &str, name: &str) -> String {
    format!("'{value}' for '{name}' is not a valid timestamp")
}
pub fn bad_default(value: &str) -> String {
    format!("failed to format default config value: {value}")
}
pub fn invalid_color(value: &str) -> String {
    format!("invalid color value: {value}")
}
pub fn cannot_parse_color(value: &str) -> String {
    format!("cannot parse color '{value}'")
}
pub const BAD_DEFAULT_COLOR: &str = "unable to parse default color value";
pub fn bad_config_line(line: usize, origin: &str) -> String {
    format!("bad config line {line} in {origin}")
}
pub fn cannot_overwrite(key: &str, new_syntax: bool) -> String {
    let hint = if new_syntax {
        "Use --value=<pattern>, --append or --all"
    } else {
        "Use a regexp, --add or --replace-all"
    };
    format!("cannot overwrite multiple values with a single value\n       {hint} to change {key}.")
}
pub fn global_template(name: &str, email: &str) -> String {
    format!("{GLOBAL_TEMPLATE_HEAD}#\tname = {name}\n#\temail = {email}\n")
}

// Option help.
pub const H_CONFIG_FILE_LOCATION: &str = "Config file location";
pub const H_USE_GLOBAL_CONFIG_FILE: &str = "use global config file";
pub const H_USE_SYSTEM_CONFIG_FILE: &str = "use system config file";
pub const H_USE_REPOSITORY_CONFIG_FILE: &str = "use repository config file";
pub const H_USE_PER_WORKTREE_CONFIG_FILE: &str = "use per-worktree config file";
pub const H_USE_GIVEN_CONFIG_FILE: &str = "use given config file";
pub const H_READ_CONFIG_FROM_GIVEN_BLOB: &str = "read config from given blob object";
pub const H_DISPLAY_OPTIONS: &str = "Display options";
pub const H_TERMINATE_VALUES_WITH_NUL_BYTE: &str = "terminate values with NUL byte";
pub const H_SHOW_VARIABLE_NAMES_ONLY: &str = "show variable names only";
pub const H_SHOW_ORIGIN_OF_CONFIG_FILE: &str =
    "show origin of config (file, standard input, blob, command line)";
pub const H_SHOW_SCOPE_OF_CONFIG_WORKTREE: &str =
    "show scope of config (worktree, local, global, system, command)";
pub const H_SHOW_CONFIG_KEYS_IN_ADDITION: &str = "show config keys in addition to their values";
pub const H_VALUE_IS_GIVEN_THIS_TYPE: &str = "value is given this type";
pub const H_VALUE_IS_TRUE_OR_FALSE: &str = "value is \"true\" or \"false\"";
pub const H_VALUE_IS_DECIMAL_NUMBER: &str = "value is decimal number";
pub const H_VALUE_IS_BOOL_OR_INT: &str = "value is --bool or --int";
pub const H_VALUE_IS_BOOL_OR_STRING: &str = "value is --bool or string";
pub const H_VALUE_IS_A_PATH_FILE: &str = "value is a path (file or directory name)";
pub const H_VALUE_IS_AN_EXPIRY_DATE: &str = "value is an expiry date";
pub const H_RESPECT_INCLUDE_DIRECTIVES_ON_LOOKUP: &str = "respect include directives on lookup";
pub const H_USE_STRING_EQUALITY_WHEN_COMPARING: &str =
    "use string equality when comparing values to value pattern";
pub const H_HUMAN_READABLE_COMMENT_STRING_WILL: &str =
    "human-readable comment string (# will be prepended as needed)";
pub const H_GET_VALUE_NAME_VALUE_PATTERN: &str = "get value: name [<value-pattern>]";
pub const H_GET_ALL_VALUES_KEY_VALUE: &str = "get all values: key [<value-pattern>]";
pub const H_GET_VALUES_FOR_REGEXP_NAME: &str =
    "get values for regexp: name-regex [<value-pattern>]";
pub const H_GET_VALUE_SPECIFIC_FOR_THE: &str = "get value specific for the URL: section[.var] URL";
pub const H_REPLACE_ALL_MATCHING_VARIABLES_NAME: &str =
    "replace all matching variables: name value [<value-pattern>]";
pub const H_ADD_A_NEW_VARIABLE_NAME: &str = "add a new variable: name value";
pub const H_REMOVE_A_VARIABLE_NAME_VALUE: &str = "remove a variable: name [<value-pattern>]";
pub const H_REMOVE_ALL_MATCHES_NAME_VALUE: &str = "remove all matches: name [<value-pattern>]";
pub const H_RENAME_SECTION_OLD_NAME_NEW: &str = "rename section: old-name new-name";
pub const H_REMOVE_A_SECTION_NAME: &str = "remove a section: name";
pub const H_OPEN_AN_EDITOR: &str = "open an editor";
pub const H_FIND_THE_COLOR_CONFIGURED_SLOT: &str = "find the color configured: slot [<default>]";
pub const H_FIND_THE_COLOR_SETTING_SLOT: &str = "find the color setting: slot [<stdout-is-tty>]";
pub const H_WITH_GET_USE_DEFAULT_VALUE: &str = "with --get, use default value when missing entry";
pub const H_FILTER_OPTIONS: &str = "Filter options";
pub const H_RETURN_ALL_VALUES_FOR_MULTI: &str = "return all values for multi-valued config options";
pub const H_INTERPRET_THE_NAME_AS_A: &str = "interpret the name as a regular expression";
pub const H_SHOW_CONFIG_WITH_VALUES_MATCHING: &str = "show config with values matching the pattern";
pub const H_SHOW_CONFIG_MATCHING_THE_GIVEN: &str = "show config matching the given URL";
pub const H_USE_DEFAULT_VALUE_WHEN_MISSING: &str = "use default value when missing entry";
pub const H_REPLACE_MULTI_VALUED_CONFIG_OPTION: &str =
    "replace multi-valued config option with new value";
pub const H_ADD_A_NEW_LINE_WITHOUT: &str = "add a new line without altering any existing values";
pub const H_UNSET_ALL_MULTI_VALUED_CONFIG: &str = "unset all multi-valued config options";
pub const H_UNSET_MULTI_VALUED_CONFIG_OPTIONS: &str =
    "unset multi-valued config options with matching values";
