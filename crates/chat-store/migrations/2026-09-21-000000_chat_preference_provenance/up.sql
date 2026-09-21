-- A live message may create a chat before HistorySync supplies its preference
-- snapshot. Keep that snapshot renewable until an explicit app-state action
-- has spoken for each preference; its false/NULL answer is authoritative too.
ALTER TABLE chats ADD COLUMN mute_appstate_seen BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE chats ADD COLUMN archive_appstate_seen BOOLEAN NOT NULL DEFAULT FALSE;
