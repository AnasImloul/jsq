(reduce .users[] as $u ({}; .[$u.user_id|tostring] = $u.tier)) as $tier
| reduce .events[] as $e ({}; .[$tier[$e.user_id|tostring]] += $e.amount)
| to_entries | map({tier: .key, revenue: .value}) | sort_by(.tier) | .[]
