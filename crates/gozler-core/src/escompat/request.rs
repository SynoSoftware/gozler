//! The request gozler sends for a command line.

use super::{EsPlan, ScopeFolder};
use crate::output::ResultFormat;
use crate::proto::{Returns, Rows, SearchRequest, Want};
use crate::query::{Clause, FolderChoice, QueryError, QueryStart, TermMatch, Test};
use crate::search::{Search, SearchInput, TextMatch};
use crate::types::EsColumn;
use crate::winpath::{absolute, parent_of};

/// The request for `plan`: its query parsed, its folders made absolute
/// against `working_dir`, an absolute path such as `C:\repo`, its `dm:`
/// values resolved against `query_start`, and either the rows to show or,
/// for a count, the totals.
///
/// The folders of `-path`, `-parent-path` and `-parent`, and the relative
/// paths in the query, are joined to `working_dir` by
/// [`absolute`]. A drive-relative folder such as
/// `D:x` depends on the current folder of each drive, which only Windows
/// knows, so the caller resolves it first (see
/// [`is_drive_relative`](crate::winpath::is_drive_relative)).
pub fn build_search(
    plan: &EsPlan,
    working_dir: &str,
    query_start: &QueryStart,
) -> Result<SearchRequest, QueryError> {
    let json = matches!(plan.result_format(), ResultFormat::Json);
    // Always fetch the size and date; output decides what to show. JSON
    // includes creation dates; table metadata follows its columns.
    let mut wants = vec![Want::Size, Want::DateModified];
    if plan.columns.contains(&EsColumn::DateCreated) || json {
        wants.push(Want::DateCreated);
    }
    if plan.columns.contains(&EsColumn::Attributes) {
        wants.push(Want::Attributes);
    }
    if plan.columns.contains(&EsColumn::Files) || plan.columns.contains(&EsColumn::Folders) {
        wants.push(Want::Counts);
    }
    if plan.columns.contains(&EsColumn::Writes) || plan.columns.contains(&EsColumn::Written) {
        wants.push(Want::Writes);
    }
    if plan.file_id && json {
        wants.push(Want::Identity);
    }
    let returns = match plan.count {
        Some(_) => Returns::Totals,
        None => Returns::Rows(Rows {
            sort: plan.sort,
            offset: 0,
            limit: plan.row_limit(),
            wants,
        }),
    };
    let scope = plan.scope.as_ref().map(|scope| match scope {
        ScopeFolder::Dir(dir) => absolute(dir, working_dir),
        ScopeFolder::ParentOf(path) => parent_of(&absolute(path, working_dir)),
    });
    let parent = plan.parent.as_ref().map(|dir| absolute(dir, working_dir));
    let mut search = Search::new(&SearchInput {
        text: &plan.query,
        text_match: TextMatch::Terms(if plan.whole_word {
            TermMatch::WholeWords
        } else {
            TermMatch::Anywhere
        }),
        match_case: plan.match_case,
        match_path: plan.match_path,
        name_wildcards: false,
        ordered_terms: false,
        filters: &[],
        regexes: &plan.regexes,
        scope: scope.as_deref(),
        parent: parent.as_deref(),
        working_dir: Some(working_dir),
        query_start,
    })?;
    if !plan.folders.is_empty() {
        let folders = plan
            .folders
            .iter()
            .map(|folder| FolderChoice {
                path: absolute(&folder.path, working_dir),
                left_out: folder.left_out,
            })
            .collect();
        search
            .query
            .clauses
            .push(Clause::single(Test::Folders(folders)));
    }
    Ok(SearchRequest {
        my_permissions: plan.my_permissions,
        ..search.request(returns)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escompat::{Command, parse_argv};
    use crate::winpath::is_drive_relative;

    /// The request for `plan` in `working_dir`, at an arbitrary query start.
    fn search_in(plan: &EsPlan, working_dir: &str) -> Result<SearchRequest, QueryError> {
        let start = QueryStart {
            week_start: jiff::civil::Weekday::Monday,
            now: jiff::Timestamp::UNIX_EPOCH,
            time_zone: jiff::tz::TimeZone::UTC,
        };
        build_search(plan, working_dir, &start)
    }

    fn plan(tokens: &[&str]) -> EsPlan {
        let args: Vec<String> = tokens.iter().map(ToString::to_string).collect();
        match parse_argv(&args) {
            Ok(Command::Run(plan)) => plan,
            other => panic!("{tokens:?}: {other:?}"),
        }
    }

    /// The daemon looks each component of a scope up as a child folder and
    /// routes the scope by its drive, so every form a user types must arrive
    /// absolute, without `.` or `..`.
    #[test]
    fn a_scope_is_made_absolute_with_windows_rules() {
        let cases = [
            (r"C:\repo\docs\", r"D:\other", r"C:\repo\docs"),
            ("C:/repo/./src/../docs", r"D:\other", r"C:\repo\docs"),
            (".", r"C:\repo\src", r"C:\repo\src"),
            (r"..\docs", r"C:\repo\src", r"C:\repo\docs"),
            (r"\tmp", r"C:\repo\src", r"C:\tmp"),
            (r"C:\..\..", r"D:\other", r"C:\"),
            (r"\\server\share\a\..\b", r"C:\repo", r"\\server\share\b"),
            (r"\x", r"\\server\share\dir", r"\\server\share\x"),
            (r"\\?\Volume{1}\a\..", r"C:\repo", r"\\?\Volume{1}\a\.."),
            ("c:x", r"C:\repo", r"C:\repo\x"),
            ("D:x", r"C:\repo", r"D:\x"),
        ];
        for (scope, working_dir, absolute) in cases {
            let request = search_in(&plan(&["-path", scope]), working_dir).expect("parses");
            assert_eq!(
                request.scope.as_deref(),
                Some(absolute),
                "{scope} in {working_dir}"
            );
        }
        assert!(is_drive_relative("D:x") && is_drive_relative("D:"));
        assert!(!is_drive_relative(r"D:\x") && !is_drive_relative("x"));
    }

    /// `-parent-path` searches below the folder that holds its path, and a
    /// root holds itself. `-parent` adds its test and is the scope only when
    /// no other switch names one, so `-path` still decides the volumes.
    #[test]
    fn the_parent_switches_name_their_folders() {
        let search = |tokens: &[&str]| search_in(&plan(tokens), r"C:\repo\src").expect("parses");
        let scope = |tokens: &[&str]| search(tokens).scope;
        assert_eq!(
            scope(&["-parent-path", "main.rs"]).as_deref(),
            Some(r"C:\repo\src")
        );
        assert_eq!(scope(&["-parent-path", "C:/"]).as_deref(), Some(r"C:\"));

        let parent = search(&["-parent", ".."]);
        assert_eq!(parent.scope.as_deref(), Some(r"C:\repo"));
        assert_eq!(
            parent.query.clauses,
            [Clause::single(Test::Parent(r"C:\repo".to_string()))]
        );
        assert_eq!(
            scope(&["-parent", "..", "-path", r"D:\x"]).as_deref(),
            Some(r"D:\x")
        );
    }

    /// The daemon tests real full paths, so a folder typed relative must
    /// arrive absolute, or it would leave out nothing.
    #[test]
    fn the_folder_switches_name_absolute_folders() {
        let request = search_in(
            &plan(&[
                "--leave-out",
                r"..\docs",
                "--leave-in",
                r"C:\repo\docs\keep",
            ]),
            r"C:\repo\src",
        )
        .expect("parses");
        assert_eq!(
            request.query.clauses,
            [Clause::single(Test::Folders(vec![
                FolderChoice {
                    path: r"C:\repo\docs".to_string(),
                    left_out: true,
                },
                FolderChoice {
                    path: r"C:\repo\docs\keep".to_string(),
                    left_out: false,
                },
            ]))]
        );
    }

    /// `-r` takes its whole argument as the pattern, which a `regex:` term
    /// would need quoted, and reads it as `regex:` does, -p included; `-ww`
    /// makes the query's text terms whole words.
    #[test]
    fn the_matching_switches_shape_the_query() {
        let tests = |tokens: &[&str]| -> Vec<Test> {
            search_in(&plan(tokens), r"C:\repo")
                .expect("parses")
                .query
                .clauses
                .into_iter()
                .flat_map(|clause| clause.terms)
                .map(|term| term.test)
                .collect()
        };
        assert_eq!(
            tests(&["-r", r#"^"a b"|c$"#, "-r", r"x\\y"]),
            [
                Test::NameRegex(r#"^"a b"|c$"#.to_string()),
                Test::PathRegex(r"x\\y".to_string()),
            ]
        );
        assert_eq!(
            tests(&["-p", "-r", "^C:"]),
            [Test::PathRegex("^C:".to_string())]
        );
        assert_eq!(
            tests(&["-ww", "report"]),
            [Test::NameWholeWord("report".to_string())]
        );
    }

    /// A count asks for the totals, which cover every match, so `-n` never
    /// reaches it; rows carry the limit.
    #[test]
    fn a_count_asks_for_the_totals_and_rows_carry_the_limit() {
        let returns = |tokens: &[&str]| search_in(&plan(tokens), r"C:\").expect("parses").returns;
        assert_eq!(returns(&["-get-result-count", "-n", "5"]), Returns::Totals);
        assert_eq!(returns(&["-get-folder-size", r"C:\x"]), Returns::Totals);
        assert!(matches!(
            returns(&["-n", "5"]),
            Returns::Rows(Rows { limit: Some(5), .. })
        ));
    }

    #[test]
    fn json_requests_creation_dates_without_adding_a_column() {
        for (tokens, want_created) in [
            (vec!["--json", "report"], true),
            (vec!["report"], false),
            (vec!["--json", "-export-csv", "report.csv", "report"], false),
        ] {
            let request = search_in(&plan(&tokens), r"C:\").expect("parses");
            assert!(matches!(
                request.returns,
                Returns::Rows(Rows { wants, .. })
                    if wants.contains(&Want::DateCreated) == want_created
            ));
        }
    }

    /// Without `--my-permissions` the search must not name the field, or an older
    /// gozlerd, which refuses a field it does not know, refuses every
    /// search. With it, the field must arrive, or a user on the list of
    /// users who see every file gets every file.
    #[test]
    fn a_search_names_my_permissions_only_when_asked() {
        let sent = |tokens: &[&str]| {
            let search = search_in(&plan(tokens), r"C:\").expect("parses");
            serde_json::to_value(crate::proto::Request::Search(search)).expect("serializes")
        };
        assert_eq!(
            sent(&["--my-permissions", "report"])["my_permissions"],
            true
        );
        assert_eq!(sent(&["report"]).get("my_permissions"), None);
    }
}
