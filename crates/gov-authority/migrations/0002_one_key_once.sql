-- A public key is enrolled once (review finding GR-116): the same key under a second id, or
-- for a second name, could be disowned under either. The key id is already the row's name.
CREATE UNIQUE INDEX authority_registry_one_key
    ON authority_registry (tenant, env, (doc -> 'key' ->> 'key'))
    WHERE kind = 'key';
