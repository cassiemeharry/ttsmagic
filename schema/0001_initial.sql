CREATE TABLE scryfall_card
( json JSONB NOT NULL
, updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX scryfall_card_id ON scryfall_card ((json ->> 'id'));

CREATE INDEX scryfall_card_oracle_id ON scryfall_card ((json ->> 'oracle_id'));

CREATE INDEX scryfall_card_name ON scryfall_card USING GIN ((string_to_array(json ->> 'name', ' // ')));

CREATE TABLE scryfall_set
( json JSONB NOT NULL
, updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX scryfall_set_id ON scryfall_set ((json ->> 'id'));

CREATE UNIQUE INDEX scryfall_set_code ON scryfall_set ((json ->> 'code'));

CREATE INDEX scryfall_set_name ON scryfall_set ((json ->> 'name'));
