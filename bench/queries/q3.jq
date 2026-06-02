reduce .events[] as $e ({}; .[$e.region] += $e.amount)
| to_entries | map({region: .key, revenue: .value}) | sort_by(.region) | .[]
