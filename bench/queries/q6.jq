(reduce .regions[] as $r ({}; .[$r.code] = $r.country)) as $country
| (reduce .users[] as $u ({}; .[$u.user_id|tostring] = $u.region)) as $ureg
| reduce .events[] as $e ({}; .[$country[$ureg[$e.user_id|tostring]]] += $e.amount)
| to_entries | map({country: .key, revenue: .value}) | sort_by(.country) | .[]
