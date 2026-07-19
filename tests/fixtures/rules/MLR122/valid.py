import manim as mn


class WorkingReadds(mn.Scene):
    def construct(self):
        tied = mn.Square(z_index=2)
        peer = mn.Circle(z_index=2)
        self.add(tied, peer)
        # Equal z_index uses the stable root tie order, so this works.
        self.bring_to_front(tied)

        top = mn.Square(z_index=4)
        lower = mn.Circle(z_index=1)
        self.add(top, lower)
        self.bring_to_front(top)

        fresh = mn.Dot(z_index=0)
        blocker = mn.Dot(z_index=5)
        self.add(blocker)
        # A newly-added object is not the re-add contradiction MLR122 owns.
        self.bring_to_front(fresh)
