CREATE INDEX scryfall_card_name_lower ON scryfall_card USING GIN ((string_to_array(lower(json ->> 'name'), ' // ')));

CREATE INDEX scryfall_card_name_upper ON scryfall_card USING GIN ((string_to_array(upper(json ->> 'name'), ' // ')));
