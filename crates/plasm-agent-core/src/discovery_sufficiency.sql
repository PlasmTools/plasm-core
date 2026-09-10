-- Forward cutover: discovery no longer owns conversational continuation.
-- Apply after historical schema migrations; session generation pins remain intact.
DROP TABLE IF EXISTS discovery_routing_receipts;
