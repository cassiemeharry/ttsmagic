-- We want to allow a card to be in multiple piles (i.e. main deck and sideboard).
ALTER TABLE deck_entry DROP CONSTRAINT deck_entry_pkey;

ALTER TABLE deck_entry ADD PRIMARY KEY (deck_id, card, pile);
