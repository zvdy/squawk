---
id: require-query-table-schema
title: require-query-table-schema
---

## problem

Without an explicit schema, Postgres uses the `search_path` when looking up tables and similar objects. This can result in ambiguity as to what specific table you're querying.

```sql
-- bad
select * from posts;

insert into posts(id) values (1);

update posts set id = 2;

delete from posts;
```

Tables referenced in sub queries, joins, `update ... from`, and `delete ... using` are checked too.

## solution

Specify the schema (i.e., `public`) alongside the table names in your queries.

```sql
-- good
select * from public.posts;

insert into public.posts(id) values (1);

update public.posts set id = 2;

delete from public.posts;
```

Common table expressions and temp tables are always referred to by their bare names, so they're allowed:

```sql
-- good
with recent as (
    select * from public.posts
)
select * from recent;

create temp table tmp_posts(id bigint);

select * from tmp_posts;
```

## links

- [require-table-schema](./require-table-schema.md) checks table names in DDL
- https://www.postgresql.org/docs/current/ddl-schemas.html#DDL-SCHEMAS-PATH
