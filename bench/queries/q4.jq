reduce .events[] as $e ({};
  .[$e.region] |= ((. // {revenue:0, n:0})
    | {revenue: (.revenue + $e.amount), n: (.n + 1)}))
| to_entries
| map({region: .key, revenue: .value.revenue, n: .value.n, avg: (.value.revenue / .value.n)})
| sort_by(-.revenue) | .[]
