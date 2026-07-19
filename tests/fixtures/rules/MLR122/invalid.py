from manim import *


class DefeatedReadd(Scene):
    def construct(self):
        low = Square(z_index=0)
        high = Circle(z_index=3)
        self.add(low, high)
        self.bring_to_front(low)
        self.wait()
