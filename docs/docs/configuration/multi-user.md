# User Records

Migration `0014_multi_user_access.sql` creates user, role, grant, and token tables. The current authentication system does not use these tables.

The current sign-in path authenticates the local `operator` administrator. The service does not authenticate users created through `/api/v1/users`.

The service does not enforce stored roles, channel grants, profile grants, or user tokens. Do not use these records as access control.

## Stored roles

| Role | Stored value |
|---|---|
| `admin` | A role value in the `users` table. |
| `operator` | A role value in the `users` table. |
| `viewer` | A role value in the `users` table. |

## Create a user record

Use this endpoint to create a user record.

```bash
curl -X POST http://localhost:8080/api/v1/users \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "username": "alice",
    "displayName": "Alice",
    "password": "a-strong-password",
    "role": "operator"
  }'
```

The endpoint accepts `admin`, `operator`, and `viewer` role values. The endpoint does not create a usable sign-in account.

## List, update, or delete user records

Use this endpoint to list user records.

```bash
curl http://localhost:8080/api/v1/users \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Use this endpoint to update one user record.

```bash
curl -X PATCH http://localhost:8080/api/v1/users/{user_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"displayName": "Alice Smith", "enabled": true}'
```

Use this endpoint to delete one user record.

```bash
curl -X DELETE http://localhost:8080/api/v1/users/{user_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Grant tables

The `user_channel_grants` table stores channel grant data. The `user_profile_grants` table stores output-profile grant data.

The current control API has no grant management endpoints. The current authorization path does not read either table.

The `user_tokens` table stores token data. The current authorization path does not validate these tokens.

## User interface

The `/users` page creates, lists, updates, and deletes user records. The page does not create multi-user access control.
