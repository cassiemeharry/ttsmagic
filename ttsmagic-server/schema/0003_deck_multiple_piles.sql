CREATE TYPE deck_pile AS ENUM ('main_deck', 'sideboard', 'commander');

ALTER TABLE deck_entry
    ADD COLUMN pile deck_pile NULL
  , ADD CONSTRAINT deck_entry_commander_single
        CHECK ((pile = 'commander' AND copies = 1) OR (pile <> 'commander'));

UPDATE deck_entry SET pile = 'main_deck' WHERE is_sideboard = FALSE;

UPDATE deck_entry SET pile = 'sideboard' WHERE is_sideboard = TRUE;

ALTER TABLE deck_entry
    ALTER COLUMN pile SET NOT NULL
  , DROP COLUMN is_sideboard;
