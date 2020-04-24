CREATE TABLE ttsmagic_user
( steam_id BIGINT NOT NULL PRIMARY KEY
, display_name TEXT NOT NULL
, last_login TIMESTAMPTZ NOT NULL
);

CREATE TABLE deck
( id UUID NOT NULL PRIMARY KEY
, user_id BIGINT NOT NULL
  REFERENCES ttsmagic_user(steam_id)
  ON DELETE CASCADE
  ON UPDATE CASCADE
, title TEXT NOT NULL
, url TEXT NOT NULL
, json jsonb NULL DEFAULT NULL
, UNIQUE ( user_id, url )
);

CREATE TABLE deck_entry
( deck_id UUID NOT NULL
  REFERENCES deck (id)
  ON DELETE CASCADE
  ON UPDATE CASCADE
, card UUID NOT NULL -- unfortunately, we can't have a FK into JSON, even if it's indexed
, copies SMALLINT NOT NULL CHECK (copies >= 1)
, is_sideboard BOOLEAN NOT NULL DEFAULT FALSE
, PRIMARY KEY ( deck_id, card )
);
