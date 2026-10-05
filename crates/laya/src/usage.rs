//! Shared selection of immutable usage evidence; no inferred token counts.
use serde_json::{json,Value};
use std::collections::{HashMap,HashSet};

pub(crate) struct Selection<'a> {
    pub report:Option<&'a Value>,
    pub exclusions:HashMap<&'static str,u64>,
}

pub(crate) fn select<'a>(group:&[&'a Value],reused_streams:&HashSet<String>)->Selection<'a> {
    let mut ordered:HashMap<&str,Vec<&Value>>=HashMap::new();
    let mut legacy=Vec::new();
    let mut exclusions=HashMap::new();
    for &report in group {
        if let Some(stream)=report["usage_stream_id"].as_str() {ordered.entry(stream).or_default().push(report);}
        else if report["scope"]=="attempt" {legacy.push(report);}
        else if let Some(reason)=exclusion_reason(report) {*exclusions.entry(reason).or_default()+=1;}
    }
    let report=if ordered.len()>1 {
        *exclusions.entry("multiple_ordered_streams").or_default()+=ordered.values().map(Vec::len).sum::<usize>() as u64;
        None
    } else if !ordered.is_empty()&&!legacy.is_empty() {
        *exclusions.entry("mixed_ordered_and_legacy").or_default()+=ordered.values().map(Vec::len).sum::<usize>() as u64+legacy.len() as u64;
        None
    } else if let Some((stream_id,stream))=ordered.into_iter().next() {
        if reused_streams.contains(stream_id) {
            *exclusions.entry("usage_stream_reused").or_default()+=stream.len() as u64;
            None
        } else {
            let mut sequences:HashMap<u64,&Value>=HashMap::new();
            let mut conflict=false;
            for report in &stream {
                let sequence=report["source_sequence"].as_u64().expect("validated usage source_sequence");
                if sequences.get(&sequence).is_some_and(|existing|*existing!=*report){conflict=true;}
                else {sequences.entry(sequence).or_insert(report);}
            }
            if conflict {
                *exclusions.entry("ordered_sequence_conflict").or_default()+=stream.len() as u64;
                None
            } else if let Some((_,report))=sequences.into_iter().max_by_key(|(sequence,_)|*sequence) {
                if let Some(reason)=exclusion_reason(report) {*exclusions.entry(reason).or_default()+=1;None}else{Some(report)}
            } else {None}
        }
    } else if legacy.is_empty() {None}
    else if legacy.iter().all(|report|legacy_signature(report)==legacy_signature(legacy[0])) {
        if let Some(reason)=exclusion_reason(legacy[0]) {*exclusions.entry(reason).or_default()+=1;None}else{Some(legacy[0])}
    } else {
        *exclusions.entry("legacy_reports_ambiguous").or_default()+=legacy.len() as u64;
        None
    };
    Selection {report,exclusions}
}

fn legacy_signature(report:&Value)->Value {
    json!({"total_tokens":report.get("total_tokens"),"input_tokens":report.get("input_tokens"),
        "output_tokens":report.get("output_tokens"),"source":report.get("source"),
        "source_verified":report.get("source_verified"),"scope":report.get("scope"),
        "checkpoint":report.get("checkpoint"),"parent_scope":report.get("parent_scope"),
        "overlap_status":report.get("overlap_status")})
}

fn exclusion_reason(report:&Value)->Option<&'static str> {
    if report["source_verified"]!=true {Some("unverified_source")}
    else if report["scope"]!="attempt" {Some("ineligible_scope")}
    else if report["overlap_status"]!="non_overlapping" {Some("overlap_not_non_overlapping")}
    else if report["total_tokens"].as_u64().is_none() {Some("unknown_total_tokens")}
    else {None}
}
