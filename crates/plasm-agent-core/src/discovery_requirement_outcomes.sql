-- Binary cutover: previous receipt JSON and scalar answers are not compatible.
-- migrate() holds the discovery migration advisory lock. Only drain on first upgrade.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
               WHERE table_schema = current_schema()
                 AND table_name = 'discovery_routing_receipts'
                 AND column_name = 'chosen_alternative') THEN
        DELETE FROM discovery_routing_receipts;
        ALTER TABLE discovery_routing_receipts DROP COLUMN chosen_alternative;
    END IF;
END $$;
ALTER TABLE discovery_routing_receipts ADD COLUMN IF NOT EXISTS chosen_alternatives integer[]
    CHECK (cardinality(chosen_alternatives) > 0 AND 0 < ALL(chosen_alternatives)
           AND array_position(chosen_alternatives, NULL) IS NULL);
