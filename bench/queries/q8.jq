(reduce .users[] as $u ({}; .[$u.user_id|tostring] = $u.tier)) as $tier
| reduce (.events[] | select(.status == "paid")) as $e ({};
    reduce $e.items[] as $it (.;
      .[$tier[$e.user_id|tostring]] |= ((. // {lines:0, qty:0})
        | .lines += 1 | .qty += $it.qty)))
| to_entries
| map({tier: .key, lines: .value.lines, qty: .value.qty})
| sort_by(-.qty) | .[]
