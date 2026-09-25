# Executable admitted Python frontend specimen; declarations are supplied by Plasm.
# Host declarations supply Program, compute, Value, e1 and e2.
# e1 = abstract Item; e2 = abstract Tag; e1.r1 = Item.tags.


class ExportItems(Program):
    """Return each item's title and joined tag labels."""

    @compute
    def joined_labels(self, tags: list[Value[e2]]) -> str:
        return "|".join(tag.label for tag in tags)

    def build(self):
        items = e1.query()
        return items.map(
            lambda item: {
                "title": item.title,
                "labels": self.joined_labels(item.r1),
            },
            max_parents=256,
        )
