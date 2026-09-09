# Vinted sales fixtures

These are synthetic, source-grounded fixtures, not captured account payloads.
Values are fictional. The schema comes from the Android transaction list:

- `TransactionListApi.getMyOrdersMessages`: `GET my_orders`.
- `TransactionListRequest.toParams`: `type`, `status`, `page`, `per_page`.
- `OrderType` and `OrderStatus`: sold orders and the four supported filters.
- `MyOrderMessagesResponse`: `myOrders` and `pagination`.
- `MyOrder`, `PaginationState`, and `Money`: the nullable order fields,
  transaction identity, pagination counters, and monetary fields.
- `SerializationModule`: lower-case-with-underscores Gson naming, producing
  `my_orders`, `transaction_id`, `currency_code`, and pagination wire keys.
- `TransactionListApiModule`, `ApplicationGraph`, and `RestAdapterModule` bind
  the API to API v2. `vinted_api_root_v2` in Android resources is `%s/api/v2/`.

The source snapshot is under the ignored
`history/2026-08-23-vinted-android/jadx/` directory. Extra private fields exercise
allowlisted normalization; they are not claims about upstream field presence.
