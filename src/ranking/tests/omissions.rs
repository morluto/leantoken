use super::*;

#[test]
fn omission_facets_fold_long_tails_into_other() {
    let counts = (0..20)
        .map(|index| (format!("path-{index:02}"), 1))
        .collect();

    let facets = bounded_facets(counts);

    assert_eq!(facets.len(), MAX_OMISSION_FACETS);
    assert_eq!(facets.last().expect("other").value, "[other]");
    assert_eq!(facets.last().expect("other").count, 9);
    assert_eq!(facets.iter().map(|facet| facet.count).sum::<usize>(), 20);
}
