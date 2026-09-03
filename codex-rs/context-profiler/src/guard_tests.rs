//! Keeps committed test sources free of home-directory paths.

const SOURCES: [(&str, &str); 4] = [
    ("profiler_tests.rs", include_str!("profiler_tests.rs")),
    ("fixture_tests.rs", include_str!("fixture_tests.rs")),
    ("classify_tests.rs", include_str!("classify_tests.rs")),
    ("estimate_tests.rs", include_str!("estimate_tests.rs")),
];

#[test]
fn test_sources_contain_no_home_paths() {
    let needles = [
        format!("/Use{}", "rs/"),
        format!("/ho{}", "me/"),
        format!("C:\\Us{}", "ers"),
    ];
    let needles = &needles;
    let offenders: Vec<String> = SOURCES
        .iter()
        .flat_map(|(name, source)| {
            source
                .lines()
                .enumerate()
                .filter(move |(_, line)| {
                    needles.iter().any(|needle| line.contains(needle.as_str()))
                })
                .map(move |(index, _)| format!("{name}:{}", index + 1))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "home paths in test sources: {offenders:?}"
    );
}
