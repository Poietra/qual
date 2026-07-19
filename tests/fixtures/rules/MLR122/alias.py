import manim as mn


class AliasedReadd(mn.Scene):
    def construct(self):
        low = mn.Square(z_index=0)
        high = mn.Circle(z_index=3)
        self.add(low, high)
        self.bring_to_front(low)
