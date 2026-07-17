use std::collections::{BTreeMap, HashMap};

use super::{Candidate, DynError, Language, PrepareConfig};

pub(super) fn query_contains_exact_identifier(query: &str) -> bool {
    let locus = query
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .trim_start_matches('#')
        .trim();
    if backtick_spans(locus).any(is_code_like_token) {
        return true;
    }
    locus
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|character: char| {
                !character.is_alphanumeric()
                    && !matches!(character, '_' | '.' | ':' | '/' | '#' | '-' | '[' | ']')
            })
        })
        .filter(|token| !token.starts_with("http://") && !token.starts_with("https://"))
        .any(is_code_like_token)
}

fn backtick_spans(query: &str) -> impl Iterator<Item = &str> {
    query
        .split('`')
        .enumerate()
        .filter(|(index, value)| index % 2 == 1 && !value.trim().is_empty())
        .map(|(_, value)| value.trim())
}

fn is_code_like_token(token: &str) -> bool {
    if token.len() < 2 || token.chars().any(char::is_whitespace) {
        return false;
    }
    token.contains("::")
        || token.contains("#[")
        || token.contains('_')
        || token.contains('/')
        || token.contains('.')
            && !token.ends_with('.')
            && !token.starts_with("www.")
            && ![".com", ".org", ".net", ".io", ".dev"]
                .iter()
                .any(|suffix| token.ends_with(suffix))
        || token
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
}

pub(super) fn selection_key(seed: &str, language: Language, task_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(seed.as_bytes());
    hasher.update(&[0]);
    hasher.update(language.as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(task_id.as_bytes());
    hasher.finalize().to_hex().to_string()
}

pub(super) fn select_candidates(
    candidates: Vec<Candidate>,
    config: &PrepareConfig<'_>,
) -> Result<Vec<Candidate>, DynError> {
    let mut by_language = BTreeMap::<Language, Vec<Candidate>>::new();
    for candidate in candidates {
        by_language
            .entry(candidate.language)
            .or_default()
            .push(candidate);
    }
    let mut selected = Vec::new();
    for language in &config.languages {
        let mut available = by_language.remove(language).unwrap_or_default();
        available.sort_by(|left, right| {
            left.selection_key
                .cmp(&right.selection_key)
                .then_with(|| left.task.task_id.cmp(&right.task.task_id))
        });
        let mut chosen = Vec::new();
        let exact_quota = config.tasks_per_language - config.non_exact_per_language;
        let exact_available = available
            .iter()
            .filter(|candidate| candidate.exact_identifier)
            .count();
        let non_exact_available = available.len() - exact_available;
        let mut exact_suffix = vec![0usize; available.len() + 1];
        let mut non_exact_suffix = vec![0usize; available.len() + 1];
        for index in (0..available.len()).rev() {
            exact_suffix[index] =
                exact_suffix[index + 1] + usize::from(available[index].exact_identifier);
            non_exact_suffix[index] =
                non_exact_suffix[index + 1] + usize::from(!available[index].exact_identifier);
        }
        if !choose_stratified_subset(
            &available,
            0,
            exact_quota,
            config.non_exact_per_language,
            config.max_tasks_per_repository,
            &exact_suffix,
            &non_exact_suffix,
            &mut HashMap::new(),
            &mut chosen,
        ) {
            return Err(format!(
                "{} cannot satisfy exact/non-exact quotas {exact_quota}/{} from {exact_available}/{non_exact_available} available tasks with repository cap {}",
                language.as_str(),
                config.non_exact_per_language,
                config.max_tasks_per_repository
            )
            .into());
        }
        if chosen.len() != config.tasks_per_language {
            return Err(format!(
                "{} selection produced {} tasks instead of {}",
                language.as_str(),
                chosen.len(),
                config.tasks_per_language
            )
            .into());
        }
        chosen.sort_unstable();
        selected.extend(
            available
                .into_iter()
                .enumerate()
                .filter_map(|(index, candidate)| {
                    chosen.binary_search(&index).ok().map(|_| candidate)
                }),
        );
    }
    selected.sort_by(|left, right| {
        left.language
            .cmp(&right.language)
            .then_with(|| left.selection_key.cmp(&right.selection_key))
    });
    Ok(selected)
}

#[allow(clippy::too_many_arguments)]
fn choose_stratified_subset<'a>(
    candidates: &'a [Candidate],
    index: usize,
    exact_remaining: usize,
    non_exact_remaining: usize,
    repository_cap: usize,
    exact_suffix: &[usize],
    non_exact_suffix: &[usize],
    repositories: &mut HashMap<&'a str, usize>,
    chosen_indexes: &mut Vec<usize>,
) -> bool {
    if exact_remaining == 0 && non_exact_remaining == 0 {
        return true;
    }
    if index == candidates.len()
        || exact_suffix[index] < exact_remaining
        || non_exact_suffix[index] < non_exact_remaining
    {
        return false;
    }

    let candidate = &candidates[index];
    let needed = if candidate.exact_identifier {
        exact_remaining > 0
    } else {
        non_exact_remaining > 0
    };
    let repository = candidate.repository.as_str();
    let previous_count = repositories.get(repository).copied().unwrap_or(0);
    if needed && previous_count < repository_cap {
        repositories.insert(repository, previous_count + 1);
        chosen_indexes.push(index);
        let found = choose_stratified_subset(
            candidates,
            index + 1,
            exact_remaining - usize::from(candidate.exact_identifier),
            non_exact_remaining - usize::from(!candidate.exact_identifier),
            repository_cap,
            exact_suffix,
            non_exact_suffix,
            repositories,
            chosen_indexes,
        );
        if found {
            return true;
        }
        chosen_indexes.pop();
        if previous_count == 0 {
            repositories.remove(repository);
        } else {
            repositories.insert(repository, previous_count);
        }
    }
    choose_stratified_subset(
        candidates,
        index + 1,
        exact_remaining,
        non_exact_remaining,
        repository_cap,
        exact_suffix,
        non_exact_suffix,
        repositories,
        chosen_indexes,
    )
}
