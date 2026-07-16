#[path = "support/wire_trace.rs"]
mod wire_trace;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    fs,
    path::PathBuf,
};

use clap::{Parser, ValueEnum};
use leantoken::tokens::Tokenizer;
use serde::Serialize;
use wire_trace::{Direction, Event, ProviderUsage, RangeIdentity, TRACE_SCHEMA_V1, Trace};

#[derive(Debug, Parser)]
#[command(about = "Analyze a captured MCP JSON-RPC exchange")]
struct Args {
    /// Host trace containing exact JSON-RPC messages in observed order.
    #[arg(long)]
    trace: PathBuf,
    /// Optional report path.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize)]
struct CategoryCost {
    events: usize,
    serialized_json_tokens: usize,
    provider_visible_payload_tokens: usize,
    provider_usage: ProviderUsage,
}

#[derive(Debug, Default, Serialize)]
struct ComponentCost {
    occurrences: usize,
    local_tokens: usize,
}

#[derive(Debug, Serialize)]
struct ResultLifetime {
    result_id: String,
    tool_name: Option<String>,
    ranges: Vec<RangeIdentity>,
    first_emitted_turn: Option<u64>,
    last_visible_turn: Option<u64>,
    visible_turn_count: Option<u64>,
    serialized_result_tokens: usize,
    provider_visible_result_tokens: Option<usize>,
    source_tokens: Option<usize>,
    serialized_token_amplification: Option<u64>,
    provider_visible_token_amplification: Option<u64>,
    source_token_amplification: Option<u64>,
}

#[derive(Debug, Default, Serialize)]
struct CacheAnnotations {
    stable_prefix_events: usize,
    unstable_prefix_events: usize,
    cache_eligible_events: usize,
    cache_ineligible_events: usize,
    unknown_stable_prefix_events: usize,
    unknown_cache_eligibility_events: usize,
}

#[derive(Debug, Serialize)]
struct LatencySummary {
    samples: usize,
    total_ms: f64,
    first_timestamp_unix_millis: Option<u64>,
    last_timestamp_unix_millis: Option<u64>,
}

#[derive(Debug, Serialize)]
struct Limitation {
    code: &'static str,
    detail: &'static str,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    input_schema_version: u32,
    trace_id: String,
    trace_file_blake3: String,
    trace_content_blake3: String,
    declared_trace_content_blake3: Option<String>,
    host: String,
    host_version: String,
    model: Option<String>,
    provider: Option<String>,
    repository: Option<wire_trace::RepositoryIdentity>,
    tokenizer: &'static str,
    token_count_exact: bool,
    event_count: usize,
    total_serialized_json_tokens: usize,
    total_provider_visible_payload_tokens: usize,
    total_handoff_tokens: usize,
    total_observed_boundary_tokens: usize,
    provider_usage: ProviderUsage,
    event_categories: BTreeMap<String, CategoryCost>,
    components: BTreeMap<String, ComponentCost>,
    component_costs_overlap: bool,
    tool_result_modes: BTreeMap<String, usize>,
    exact_text_structured_duplicates: usize,
    duplicated_result_tokens: usize,
    cache_annotations: CacheAnnotations,
    latency: LatencySummary,
    result_lifetimes: Vec<ResultLifetime>,
    range_identity_count: usize,
    reread_ranges: usize,
    reread_source_tokens: usize,
    superseded_hash_ranges: usize,
    stale_generation_ranges: usize,
    required_exchange_parts: BTreeMap<&'static str, bool>,
    limitations: Vec<Limitation>,
}

#[derive(Debug, Default)]
struct UsageAccumulator {
    events: usize,
    uncached_input_tokens: OptionalSum,
    cache_creation_input_tokens: OptionalSum,
    cache_read_input_tokens: OptionalSum,
    output_tokens: OptionalSum,
    reasoning_tokens: OptionalSum,
}

#[derive(Debug, Default)]
struct OptionalSum {
    total: u64,
    complete: bool,
}

impl OptionalSum {
    fn add(&mut self, value: Option<u64>, first: bool) {
        if first {
            self.complete = true;
        }
        match value {
            Some(value) if self.complete => {
                self.total = self.total.saturating_add(value);
            }
            Some(_) => {}
            None => self.complete = false,
        }
    }

    fn finish(&self, events: usize) -> Option<u64> {
        (events > 0 && self.complete).then_some(self.total)
    }
}

impl UsageAccumulator {
    fn add(&mut self, usage: Option<&ProviderUsage>) {
        let first = self.events == 0;
        self.events += 1;
        self.uncached_input_tokens
            .add(usage.and_then(|value| value.uncached_input_tokens), first);
        self.cache_creation_input_tokens.add(
            usage.and_then(|value| value.cache_creation_input_tokens),
            first,
        );
        self.cache_read_input_tokens
            .add(usage.and_then(|value| value.cache_read_input_tokens), first);
        self.output_tokens
            .add(usage.and_then(|value| value.output_tokens), first);
        self.reasoning_tokens
            .add(usage.and_then(|value| value.reasoning_tokens), first);
    }

    fn finish(&self) -> ProviderUsage {
        ProviderUsage {
            uncached_input_tokens: self.uncached_input_tokens.finish(self.events),
            cache_creation_input_tokens: self.cache_creation_input_tokens.finish(self.events),
            cache_read_input_tokens: self.cache_read_input_tokens.finish(self.events),
            output_tokens: self.output_tokens.finish(self.events),
            reasoning_tokens: self.reasoning_tokens.finish(self.events),
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let trace_json = fs::read_to_string(&args.trace)?;
    let trace_file_blake3 = blake3::hash(trace_json.as_bytes()).to_hex().to_string();
    let trace: Trace = serde_json::from_str(&trace_json)?;
    trace
        .validate_version()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let tokenizer = Tokenizer::from_str(&trace.tokenizer, false)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let report = analyze_trace(trace, trace_file_blake3, &tokenizer)?;
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(output) = args.output {
        if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, &json)?;
    }
    println!("{json}");
    Ok(())
}

fn analyze_trace(
    trace: Trace,
    trace_file_blake3: String,
    tokenizer: &Tokenizer,
) -> Result<Report, Box<dyn Error>> {
    let trace_content_blake3 = trace.content_blake3()?;
    let declared_trace_content_blake3 = trace.trace_content_blake3.clone();
    let mut event_categories = BTreeMap::<String, CategoryCost>::new();
    let mut category_usage = BTreeMap::<String, UsageAccumulator>::new();
    let mut components = BTreeMap::<String, ComponentCost>::new();
    let mut result_modes = BTreeMap::<String, usize>::new();
    let mut required = required_exchange_parts();
    let mut total_serialized_json_tokens = 0usize;
    let mut total_provider_visible_payload_tokens = 0usize;
    let mut total_handoff_tokens = 0usize;
    let mut overall_usage = UsageAccumulator::default();
    let mut exact_text_structured_duplicates = 0usize;
    let mut duplicated_result_tokens = 0usize;
    let mut cache_annotations = CacheAnnotations::default();
    let mut latency_samples = 0usize;
    let mut total_latency_ms = 0.0;
    let mut first_timestamp = None;
    let mut last_timestamp = None;
    let mut call_tools = HashMap::<String, String>::new();
    let mut removed_at_turn = HashMap::<String, u64>::new();
    let mut parsed_messages = Vec::with_capacity(trace.events.len());

    for event in &trace.events {
        let message = event_message(event)?;
        if let Some(message) = &message
            && let Some((call_id, tool_name)) = tool_call_identity(message)
        {
            call_tools.insert(call_id, tool_name);
        }
        if let (Some(turn), Some(compaction)) = (event.turn, &event.compaction) {
            for result_id in &compaction.removed_result_ids {
                removed_at_turn.entry(result_id.clone()).or_insert(turn);
            }
        }
        parsed_messages.push(message);
    }

    let mut result_lifetimes = Vec::new();
    let mut exact_ranges = HashSet::new();
    let mut location_hashes = HashMap::<RangeLocation, String>::new();
    let mut latest_generation = HashMap::<String, u64>::new();
    let mut range_identity_count = 0usize;
    let mut reread_ranges = 0usize;
    let mut reread_source_tokens = 0usize;
    let mut superseded_hash_ranges = 0usize;
    let mut stale_generation_ranges = 0usize;

    for (event, message) in trace.events.iter().zip(&parsed_messages) {
        let serialized_json = event_serialized_json(event, message)?;
        let serialized_json_tokens = serialized_json
            .as_deref()
            .map_or(0, |value| tokenizer.count(value));
        total_serialized_json_tokens += serialized_json_tokens;
        let provider_visible_payload_tokens = event
            .provider_visible_payload
            .as_deref()
            .map_or(0, |value| tokenizer.count(value));
        total_provider_visible_payload_tokens += provider_visible_payload_tokens;
        let handoff_tokens = if matches!(event.direction, Direction::Handoff) {
            event
                .provider_visible_payload
                .as_deref()
                .map_or(serialized_json_tokens, |value| tokenizer.count(value))
        } else {
            0
        };
        total_handoff_tokens += handoff_tokens;

        let category = categorize(event, message.as_ref(), &mut required);
        let usage = event.provider_usage_compat();
        let cost = event_categories.entry(category.clone()).or_default();
        cost.events += 1;
        cost.serialized_json_tokens += serialized_json_tokens;
        cost.provider_visible_payload_tokens += provider_visible_payload_tokens;
        category_usage
            .entry(category)
            .or_default()
            .add(usage.as_ref());
        overall_usage.add(usage.as_ref());

        add_cache_annotations(&mut cache_annotations, event);
        if let Some(latency_ms) = event.latency_ms {
            latency_samples += 1;
            total_latency_ms += latency_ms;
        }
        if let Some(timestamp) = event.timestamp_unix_millis {
            first_timestamp =
                Some(first_timestamp.map_or(timestamp, |current: u64| current.min(timestamp)));
            last_timestamp =
                Some(last_timestamp.map_or(timestamp, |current: u64| current.max(timestamp)));
        }

        if matches!(event.direction, Direction::Handoff) {
            add_component(&mut components, "handoff", handoff_tokens);
        }
        if let Some(message) = message {
            analyze_components(
                message,
                tokenizer,
                &mut components,
                &mut result_modes,
                &mut exact_text_structured_duplicates,
                &mut duplicated_result_tokens,
            )?;
        }

        if !event.ranges.is_empty() {
            validate_ranges(&event.ranges)?;
            range_identity_count += event.ranges.len();
            for range in &event.ranges {
                let exact = RangeKey::from(range);
                if !exact_ranges.insert(exact) {
                    reread_ranges += 1;
                    reread_source_tokens = reread_source_tokens
                        .saturating_add(range.source_tokens.unwrap_or_default());
                }
                let location = RangeLocation::from(range);
                if location_hashes
                    .insert(location, range.content_hash.clone())
                    .is_some_and(|previous| previous != range.content_hash)
                {
                    superseded_hash_ranges += 1;
                }
                let latest = latest_generation.entry(range.path.clone()).or_default();
                if range.repository_generation < *latest {
                    stale_generation_ranges += 1;
                }
                *latest = (*latest).max(range.repository_generation);
            }
            let result_id = event
                .result_id
                .clone()
                .or_else(|| event.call_id.clone())
                .or_else(|| message.as_ref().and_then(json_rpc_id))
                .unwrap_or_else(|| format!("event-{}", event.sequence.unwrap_or_default()));
            let last_visible_turn = event.visible_through_turn.or_else(|| {
                removed_at_turn
                    .get(&result_id)
                    .map(|turn| turn.saturating_sub(1))
            });
            if let (Some(first), Some(last)) = (event.turn, last_visible_turn)
                && last < first
            {
                return Err(format!(
                    "result {result_id} is visible through turn {last} before emission at {first}"
                )
                .into());
            }
            let visible_turn_count = match (event.turn, last_visible_turn) {
                (Some(first), Some(last)) => Some(last - first + 1),
                _ => None,
            };
            let source_tokens = event
                .ranges
                .iter()
                .map(|range| range.source_tokens)
                .collect::<Option<Vec<_>>>()
                .map(|tokens| tokens.into_iter().sum::<usize>());
            let serialized_token_amplification = visible_turn_count.map(|turns| {
                u64::try_from(serialized_json_tokens)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(turns)
            });
            let provider_visible_result_tokens = event
                .provider_visible_payload
                .as_deref()
                .map(|payload| tokenizer.count(payload));
            let provider_visible_token_amplification = visible_turn_count
                .zip(provider_visible_result_tokens)
                .map(|(turns, tokens)| {
                    u64::try_from(tokens)
                        .unwrap_or(u64::MAX)
                        .saturating_mul(turns)
                });
            let source_token_amplification =
                visible_turn_count
                    .zip(source_tokens)
                    .map(|(turns, tokens)| {
                        u64::try_from(tokens)
                            .unwrap_or(u64::MAX)
                            .saturating_mul(turns)
                    });
            let tool_name = event.tool_name.clone().or_else(|| {
                event
                    .call_id
                    .as_ref()
                    .and_then(|id| call_tools.get(id).cloned())
            });
            result_lifetimes.push(ResultLifetime {
                result_id,
                tool_name,
                ranges: event.ranges.clone(),
                first_emitted_turn: event.turn,
                last_visible_turn,
                visible_turn_count,
                serialized_result_tokens: serialized_json_tokens,
                provider_visible_result_tokens,
                source_tokens,
                serialized_token_amplification,
                provider_visible_token_amplification,
                source_token_amplification,
            });
        }
    }

    for (category, usage) in category_usage {
        if let Some(cost) = event_categories.get_mut(&category) {
            cost.provider_usage = usage.finish();
        }
    }
    let provider_usage = trace
        .provider_usage
        .clone()
        .or_else(|| {
            trace
                .provider_total_input_tokens
                .map(|tokens| ProviderUsage {
                    uncached_input_tokens: Some(tokens),
                    ..ProviderUsage::default()
                })
        })
        .unwrap_or_else(|| overall_usage.finish());
    let limitations = report_limitations(&trace, &provider_usage, &result_lifetimes);
    let total_observed_boundary_tokens =
        total_serialized_json_tokens.saturating_add(total_provider_visible_payload_tokens);

    Ok(Report {
        schema_version: 2,
        input_schema_version: trace.schema_version,
        trace_id: trace
            .trace_id
            .unwrap_or_else(|| format!("blake3:{trace_file_blake3}")),
        trace_file_blake3,
        trace_content_blake3,
        declared_trace_content_blake3,
        host: trace.host,
        host_version: trace.host_version,
        model: trace.model,
        provider: trace.provider,
        repository: trace.repository,
        tokenizer: tokenizer.name(),
        token_count_exact: tokenizer.is_exact(),
        event_count: trace.events.len(),
        total_serialized_json_tokens,
        total_provider_visible_payload_tokens,
        total_handoff_tokens,
        total_observed_boundary_tokens,
        provider_usage,
        event_categories,
        components,
        component_costs_overlap: true,
        tool_result_modes: result_modes,
        exact_text_structured_duplicates,
        duplicated_result_tokens,
        cache_annotations,
        latency: LatencySummary {
            samples: latency_samples,
            total_ms: total_latency_ms,
            first_timestamp_unix_millis: first_timestamp,
            last_timestamp_unix_millis: last_timestamp,
        },
        result_lifetimes,
        range_identity_count,
        reread_ranges,
        reread_source_tokens,
        superseded_hash_ranges,
        stale_generation_ranges,
        required_exchange_parts: required,
        limitations,
    })
}

fn event_message(event: &Event) -> Result<Option<serde_json::Value>, Box<dyn Error>> {
    match (&event.raw_json, &event.message) {
        (Some(raw), _) => Ok(Some(serde_json::from_str(raw)?)),
        (None, Some(message)) => Ok(Some(message.clone())),
        (None, None) if matches!(event.direction, Direction::Handoff) => Ok(None),
        (None, None) => Err("wire event has neither raw_json nor message".into()),
    }
}

fn event_serialized_json(
    event: &Event,
    message: &Option<serde_json::Value>,
) -> Result<Option<String>, serde_json::Error> {
    match (&event.raw_json, message) {
        (Some(raw), _) => Ok(Some(raw.clone())),
        (None, Some(message)) => serde_json::to_string(message).map(Some),
        (None, None) => Ok(None),
    }
}

fn required_exchange_parts() -> BTreeMap<&'static str, bool> {
    BTreeMap::from([
        ("initialize_request", false),
        ("initialize_response", false),
        ("initialized_notification", false),
        ("tools_list", false),
        ("tools_call", false),
        ("tool_result", false),
    ])
}

fn categorize(
    event: &Event,
    message: Option<&serde_json::Value>,
    required: &mut BTreeMap<&'static str, bool>,
) -> String {
    if let Some(category) = &event.category {
        update_required(event.direction, message, required);
        return category.clone();
    }
    if matches!(event.direction, Direction::Handoff) {
        return "handoff".to_owned();
    }
    let Some(message) = message else {
        return "other".to_owned();
    };
    let method = message.get("method").and_then(serde_json::Value::as_str);
    match (event.direction, method) {
        (Direction::ClientToServer, Some("initialize")) => {
            required.insert("initialize_request", true);
            "initialize_request"
        }
        (Direction::ClientToServer, Some("notifications/initialized")) => {
            required.insert("initialized_notification", true);
            "initialized_notification"
        }
        (Direction::ClientToServer, Some("tools/list")) => {
            required.insert("tools_list", true);
            "tools_list_request"
        }
        (Direction::ClientToServer, Some("tools/call")) => {
            required.insert("tools_call", true);
            "tool_call_request"
        }
        (Direction::ServerToClient, _) if message.pointer("/result/tools").is_some() => {
            required.insert("tools_list", true);
            "tools_list_response"
        }
        (Direction::ServerToClient, _)
            if message.pointer("/result/content").is_some()
                || message.pointer("/result/structuredContent").is_some() =>
        {
            required.insert("tool_result", true);
            "tool_call_response"
        }
        (Direction::ServerToClient, _) if message.pointer("/result/protocolVersion").is_some() => {
            required.insert("initialize_response", true);
            "initialize_response"
        }
        _ => "other",
    }
    .to_owned()
}

fn update_required(
    direction: Direction,
    message: Option<&serde_json::Value>,
    required: &mut BTreeMap<&'static str, bool>,
) {
    let Some(message) = message else {
        return;
    };
    let event = Event {
        sequence: None,
        direction,
        turn: None,
        timestamp_unix_millis: None,
        latency_ms: None,
        category: None,
        message: None,
        raw_json: None,
        provider_visible_payload: None,
        tool_name: None,
        call_id: None,
        result_id: None,
        ranges: Vec::new(),
        visible_through_turn: None,
        stable_prefix: None,
        cache_eligible: None,
        compaction: None,
        provider_usage: None,
        provider_input_tokens: None,
    };
    let _ = categorize(&event, Some(message), required);
}

fn analyze_components(
    message: &serde_json::Value,
    tokenizer: &Tokenizer,
    components: &mut BTreeMap<String, ComponentCost>,
    result_modes: &mut BTreeMap<String, usize>,
    exact_duplicates: &mut usize,
    duplicated_result_tokens: &mut usize,
) -> Result<(), Box<dyn Error>> {
    if let Some(value) = message.pointer("/result/tools") {
        add_component(components, "catalog", json_tokens(tokenizer, value)?);
    }
    if let Some(value) = message.pointer("/params/arguments") {
        add_component(components, "call_arguments", json_tokens(tokenizer, value)?);
    }
    let text = message.pointer("/result/content");
    let structured = message.pointer("/result/structuredContent");
    if let Some(value) = text {
        add_component(components, "result_text", json_tokens(tokenizer, value)?);
    }
    if let Some(value) = structured {
        add_component(
            components,
            "structured_content",
            json_tokens(tokenizer, value)?,
        );
        if let Some(receipt) = value.get("receipt") {
            add_component(components, "receipt", json_tokens(tokenizer, receipt)?);
        }
    }
    if text.is_some() || structured.is_some() {
        let has_text = text
            .and_then(serde_json::Value::as_array)
            .is_some_and(|content| !content.is_empty());
        let has_structured = structured.is_some();
        let mode = match (has_text, has_structured) {
            (true, true) => "dual",
            (true, false) => "text",
            (false, true) => "structured",
            (false, false) => "empty",
        };
        *result_modes.entry(mode.to_owned()).or_default() += 1;
        if let (Some(text), Some(structured)) = (text, structured)
            && text_matches_structured(text, structured)
        {
            *exact_duplicates += 1;
            *duplicated_result_tokens += json_tokens(tokenizer, structured)?;
        }
    }
    Ok(())
}

fn text_matches_structured(text: &serde_json::Value, structured: &serde_json::Value) -> bool {
    text.as_array()
        .and_then(|items| {
            let mut texts = items.iter().filter_map(|item| {
                (item.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                    .then(|| item.get("text").and_then(serde_json::Value::as_str))
                    .flatten()
            });
            let only = texts.next()?;
            texts.next().is_none().then_some(only)
        })
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .is_some_and(|parsed| parsed == *structured)
}

fn json_tokens(
    tokenizer: &Tokenizer,
    value: &serde_json::Value,
) -> Result<usize, serde_json::Error> {
    serde_json::to_string(value).map(|json| tokenizer.count(&json))
}

fn add_component(components: &mut BTreeMap<String, ComponentCost>, category: &str, tokens: usize) {
    let cost = components.entry(category.to_owned()).or_default();
    cost.occurrences += 1;
    cost.local_tokens += tokens;
}

fn add_cache_annotations(summary: &mut CacheAnnotations, event: &Event) {
    match event.stable_prefix {
        Some(true) => summary.stable_prefix_events += 1,
        Some(false) => summary.unstable_prefix_events += 1,
        None => summary.unknown_stable_prefix_events += 1,
    }
    match event.cache_eligible {
        Some(true) => summary.cache_eligible_events += 1,
        Some(false) => summary.cache_ineligible_events += 1,
        None => summary.unknown_cache_eligibility_events += 1,
    }
}

fn tool_call_identity(message: &serde_json::Value) -> Option<(String, String)> {
    (message.get("method").and_then(serde_json::Value::as_str) == Some("tools/call"))
        .then(|| {
            Some((
                json_rpc_id(message)?,
                message
                    .pointer("/params/name")
                    .and_then(serde_json::Value::as_str)?
                    .to_owned(),
            ))
        })
        .flatten()
}

fn json_rpc_id(message: &serde_json::Value) -> Option<String> {
    let id = message.get("id")?;
    match id {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn validate_ranges(ranges: &[RangeIdentity]) -> Result<(), Box<dyn Error>> {
    for range in ranges {
        if range.path.is_empty()
            || range.start_line == 0
            || range.end_line < range.start_line
            || range.content_hash.is_empty()
        {
            return Err(format!("invalid range identity for {}", range.path).into());
        }
    }
    Ok(())
}

fn report_limitations(
    trace: &Trace,
    provider_usage: &ProviderUsage,
    result_lifetimes: &[ResultLifetime],
) -> Vec<Limitation> {
    let mut limitations = vec![
        Limitation {
            code: "transport_boundary",
            detail: "Transport bytes outside captured JSON-RPC or explicit handoff payloads are not counted.",
        },
        Limitation {
            code: "component_overlap",
            detail: "Component token categories overlap the complete event envelope and must not be summed as a second total.",
        },
    ];
    if trace.schema_version == TRACE_SCHEMA_V1 {
        limitations.push(Limitation {
            code: "legacy_schema",
            detail: "Schema v1 lacks turn, range identity, cache, latency, and compaction fields; those report values remain unknown.",
        });
    }
    if trace
        .events
        .iter()
        .all(|event| event.provider_visible_payload.is_none())
    {
        limitations.push(Limitation {
            code: "provider_framing_missing",
            detail: "No provider-visible conversation frame was exported; serialized JSON-RPC cost is local-wire evidence only.",
        });
    } else if trace.events.iter().any(|event| {
        !matches!(event.direction, Direction::Handoff) && event.provider_visible_payload.is_none()
    }) {
        limitations.push(Limitation {
            code: "provider_framing_partial",
            detail: "Only explicitly annotated provider-visible payloads cross the measured provider boundary; unannotated JSON-RPC remains local-wire evidence.",
        });
    }
    if provider_usage == &ProviderUsage::default() {
        limitations.push(Limitation {
            code: "provider_usage_missing",
            detail: "Provider-native uncached, cache creation, cache read, output, and reasoning token totals were not supplied.",
        });
    } else if [
        provider_usage.uncached_input_tokens,
        provider_usage.cache_creation_input_tokens,
        provider_usage.cache_read_input_tokens,
        provider_usage.output_tokens,
        provider_usage.reasoning_tokens,
    ]
    .contains(&None)
    {
        limitations.push(Limitation {
            code: "provider_usage_partial",
            detail: "At least one provider usage category is unavailable; missing values remain null.",
        });
    }
    if result_lifetimes.iter().any(|lifetime| {
        lifetime.first_emitted_turn.is_none() || lifetime.last_visible_turn.is_none()
    }) {
        limitations.push(Limitation {
            code: "retention_unknown",
            detail: "At least one result lacks explicit turn visibility, so its token amplification remains null.",
        });
    }
    limitations
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RangeKey {
    generation: u64,
    path: String,
    start_line: usize,
    end_line: usize,
    content_hash: String,
}

impl From<&RangeIdentity> for RangeKey {
    fn from(value: &RangeIdentity) -> Self {
        Self {
            generation: value.repository_generation,
            path: value.path.clone(),
            start_line: value.start_line,
            end_line: value.end_line,
            content_hash: value.content_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RangeLocation {
    path: String,
    start_line: usize,
    end_line: usize,
}

impl From<&RangeIdentity> for RangeLocation {
    fn from(value: &RangeIdentity) -> Self {
        Self {
            path: value.path.clone(),
            start_line: value.start_line,
            end_line: value.end_line,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire_trace::{Compaction, RepositoryIdentity, TRACE_SCHEMA_V2};

    fn test_trace(events: Vec<Event>) -> Trace {
        Trace {
            schema_version: TRACE_SCHEMA_V2,
            trace_id: Some("test".into()),
            trace_content_blake3: None,
            host: "test".into(),
            host_version: "1".into(),
            model: None,
            provider: None,
            tokenizer: "cl100k_base".into(),
            token_count_exact: Some(true),
            generated_at_unix_seconds: None,
            repository: Some(RepositoryIdentity {
                revision: "0000000000000000000000000000000000000000".into(),
                dirty_fingerprint: "clean".into(),
            }),
            final_turn: Some(3),
            provider_usage: None,
            provider_total_input_tokens: None,
            outcome: None,
            events,
        }
    }

    fn event(direction: Direction, turn: u64, message: serde_json::Value) -> Event {
        Event {
            sequence: Some(turn),
            direction,
            turn: Some(turn),
            timestamp_unix_millis: None,
            latency_ms: None,
            category: None,
            raw_json: Some(serde_json::to_string(&message).expect("serialize")),
            message: None,
            provider_visible_payload: None,
            tool_name: None,
            call_id: None,
            result_id: None,
            ranges: Vec::new(),
            visible_through_turn: None,
            stable_prefix: None,
            cache_eligible: None,
            compaction: None,
            provider_usage: None,
            provider_input_tokens: None,
        }
    }

    fn range(generation: u64, hash: &str, tokens: usize) -> RangeIdentity {
        RangeIdentity {
            repository_generation: generation,
            path: "src/lib.rs".into(),
            start_line: 10,
            end_line: 20,
            content_hash: hash.into(),
            source_tokens: Some(tokens),
        }
    }

    #[test]
    fn schema_v1_is_accepted_with_explicit_legacy_limitations() {
        let json = r#"{
            "schema_version": 1,
            "host": "legacy",
            "host_version": "1",
            "tokenizer": "cl100k_base",
            "provider_total_input_tokens": 12,
            "events": [{
                "direction": "client_to_server",
                "raw_json": "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}"
            }]
        }"#;
        let trace: Trace = serde_json::from_str(json).expect("legacy trace");
        trace.validate_version().expect("supported");
        let tokenizer = Tokenizer::default();
        let report = analyze_trace(trace, "hash".into(), &tokenizer).expect("report");
        assert_eq!(report.input_schema_version, TRACE_SCHEMA_V1);
        assert_eq!(report.provider_usage.uncached_input_tokens, Some(12));
        assert!(
            report
                .limitations
                .iter()
                .any(|limitation| limitation.code == "legacy_schema")
        );
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let mut trace = test_trace(vec![event(
            Direction::ClientToServer,
            1,
            serde_json::json!({"jsonrpc": "2.0"}),
        )]);
        trace.schema_version = 99;
        assert!(trace.validate_version().is_err());
    }

    #[test]
    fn schema_v2_round_trips_with_a_verified_content_hash() {
        let mut trace = test_trace(vec![event(
            Direction::ClientToServer,
            0,
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )]);
        trace.seal_content_hash().expect("seal trace");
        let json = serde_json::to_string(&trace).expect("serialize trace");
        let decoded: Trace = serde_json::from_str(&json).expect("deserialize trace");
        decoded.validate_version().expect("validate trace");
        assert_eq!(decoded.trace_id.as_deref(), Some("test"));
        assert_eq!(decoded.token_count_exact, Some(true));
        assert_eq!(decoded.events[0].sequence, Some(0));
        let content_hash = decoded.content_blake3().expect("content hash");
        assert_eq!(
            decoded.trace_content_blake3.as_deref(),
            Some(content_hash.as_str())
        );
    }

    #[test]
    fn three_turn_visibility_amplifies_serialized_and_source_tokens() {
        let mut result = event(
            Direction::ServerToClient,
            1,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"structuredContent": {"answer": 1}}
            }),
        );
        result.result_id = Some("result-1".into());
        result.visible_through_turn = Some(3);
        result.ranges = vec![range(1, "hash-a", 10)];
        result.provider_visible_payload = Some("{\"role\":\"tool\",\"content\":\"answer\"}".into());
        let report = analyze_trace(
            test_trace(vec![result]),
            "hash".into(),
            &Tokenizer::default(),
        )
        .expect("report");
        let lifetime = &report.result_lifetimes[0];
        assert_eq!(lifetime.visible_turn_count, Some(3));
        assert_eq!(lifetime.source_token_amplification, Some(30));
        assert_eq!(
            lifetime.serialized_token_amplification,
            Some(u64::try_from(lifetime.serialized_result_tokens).unwrap() * 3)
        );
        assert_eq!(
            lifetime.provider_visible_token_amplification,
            lifetime
                .provider_visible_result_tokens
                .map(|tokens| u64::try_from(tokens).unwrap() * 3)
        );
        assert!(report.total_provider_visible_payload_tokens > 0);
    }

    #[test]
    fn compaction_ends_result_visibility_before_the_compaction_turn() {
        let mut result = event(
            Direction::ServerToClient,
            1,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"structuredContent": {"answer": 1}}
            }),
        );
        result.result_id = Some("result-1".into());
        result.ranges = vec![range(1, "hash-a", 10)];
        let mut compaction = event(
            Direction::Handoff,
            3,
            serde_json::json!({"type": "compaction"}),
        );
        compaction.compaction = Some(Compaction {
            removed_result_ids: vec!["result-1".into()],
        });
        let report = analyze_trace(
            test_trace(vec![result, compaction]),
            "hash".into(),
            &Tokenizer::default(),
        )
        .expect("report");
        let lifetime = &report.result_lifetimes[0];
        assert_eq!(lifetime.last_visible_turn, Some(2));
        assert_eq!(lifetime.visible_turn_count, Some(2));
    }

    #[test]
    fn rereads_changed_hashes_and_stale_generations_are_distinct() {
        let mut events = Vec::new();
        for (turn, generation, hash) in [
            (1, 2, "hash-a"),
            (2, 2, "hash-a"),
            (3, 3, "hash-b"),
            (4, 1, "hash-c"),
        ] {
            let mut result = event(
                Direction::ServerToClient,
                turn,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": turn,
                    "result": {"structuredContent": {"turn": turn}}
                }),
            );
            result.ranges = vec![range(generation, hash, 5)];
            events.push(result);
        }
        let report = analyze_trace(test_trace(events), "hash".into(), &Tokenizer::default())
            .expect("report");
        assert_eq!(report.reread_ranges, 1);
        assert_eq!(report.reread_source_tokens, 5);
        assert_eq!(report.superseded_hash_ranges, 2);
        assert_eq!(report.stale_generation_ranges, 1);
    }

    #[test]
    fn partial_provider_accounting_remains_null() {
        let mut first = event(
            Direction::ClientToServer,
            1,
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        );
        first.provider_usage = Some(ProviderUsage {
            uncached_input_tokens: Some(10),
            ..ProviderUsage::default()
        });
        let second = event(
            Direction::ServerToClient,
            1,
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}}),
        );
        let report = analyze_trace(
            test_trace(vec![first, second]),
            "hash".into(),
            &Tokenizer::default(),
        )
        .expect("report");
        assert_eq!(report.provider_usage.uncached_input_tokens, None);
    }

    #[test]
    fn dual_result_duplication_is_detected_exactly() {
        let structured = serde_json::json!({"answer": 42});
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "content": [{"type": "text", "text": serde_json::to_string(&structured).unwrap()}],
                "structuredContent": structured
            }
        });
        let report = analyze_trace(
            test_trace(vec![event(Direction::ServerToClient, 1, message)]),
            "hash".into(),
            &Tokenizer::default(),
        )
        .expect("report");
        assert_eq!(report.tool_result_modes["dual"], 1);
        assert_eq!(report.exact_text_structured_duplicates, 1);
        assert!(report.duplicated_result_tokens > 0);
    }

    #[test]
    fn structured_only_result_is_classified_as_a_tool_result() {
        let trace = test_trace(vec![event(
            Direction::ServerToClient,
            1,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {"structuredContent": {"answer": 42}}
            }),
        )]);
        let report = analyze_trace(trace, "hash".into(), &Tokenizer::default()).expect("report");
        assert_eq!(report.event_categories["tool_call_response"].events, 1);
        assert!(report.required_exchange_parts["tool_result"]);
    }

    #[test]
    fn unrelated_success_response_is_not_classified_as_initialize() {
        let trace = test_trace(vec![event(
            Direction::ServerToClient,
            1,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {}
            }),
        )]);
        let report = analyze_trace(trace, "hash".into(), &Tokenizer::default()).expect("report");
        assert_eq!(report.event_categories["other"].events, 1);
        assert!(!report.required_exchange_parts["initialize_response"]);
    }

    #[test]
    fn checked_in_synthetic_token_fixture_is_deterministic() {
        let report: serde_json::Value = serde_json::from_str(include_str!(
            "../benchmarks/reports/wire-trace-synthetic-0.1.1.json"
        ))
        .expect("checked-in report");
        assert_eq!(report["total_serialized_json_tokens"], 2_001);
        assert_eq!(report["total_provider_visible_payload_tokens"], 13);
        assert_eq!(report["total_handoff_tokens"], 13);
        assert_eq!(report["total_observed_boundary_tokens"], 2_014);
        assert_eq!(
            report["result_lifetimes"][0]["serialized_token_amplification"],
            771
        );
        assert_eq!(
            report["result_lifetimes"][0]["source_token_amplification"],
            63
        );
    }
}
