# Structured monetary and trade output

Flea returns one semantic commerce model wherever draft, published listing, and
public item output includes monetary or trade data:

```yaml
trade_type: sell
price:
  kind: fixed
  amount: 5
  currency: EUR
  display: 5 €
```

`trade_type` is always one of `sell`, `give_away`, `wanted`, or `unknown`.
`unknown` means the source did not provide a recognized machine value. It must
not be inferred from `price.display`.

`price.kind` is always present and has these stable values:

| Kind | Meaning |
| --- | --- |
| `fixed` | `amount` is a JSON number. Zero remains a fixed zero price. |
| `free` | No payment is requested. `amount` is absent. |
| `negotiable` | The amount is explicitly negotiable. `amount` is absent unless the source also provides a numeric amount. |
| `not_applicable` | The trade does not define a sale price, such as a wanted listing without a budget. |
| `unavailable` | The source does not provide usable monetary machine data. |

`currency` is an uppercase three-letter currency code when the source provides
a valid code. Flea uses `EUR` when a numeric Tori amount has no separate
currency field because Tori monetary amounts are denominated in euros. Invalid
currency source values remain unavailable. `display` is optional presentation
text retained for people and diagnostics. Agents must not parse it.

Draft output applies this shape inside `values`. Published listing summaries and
details expose `trade_type` and `price` beside the listing identity. Publication
results apply the same fields to `observed_listing`. Public item details expose
the fields at the item root.

## Output compatibility

This is a deliberate structured-output shape change in the 0.1 output contract.
Published listing summaries previously returned `price` as localized text.
Listing details previously returned raw `price` and `trade_type` entries inside
`fields`, and draft `values.price` was a scalar. Callers must read the normalized
`trade_type`, `price.kind`, `price.amount`, and `price.currency` fields described
above. Presentation text is available only as optional `price.display`.

A future versioned output contract must preserve these meanings or introduce a
new output version. JSON monetary amounts remain JSON numbers in every output
format.
