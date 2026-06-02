reduce (.events[] | .items[]) as $it ({}; .[$it.sku] += $it.qty)
| to_entries | map({sku: .key, qty: .value}) | sort_by(.sku) | .[]
