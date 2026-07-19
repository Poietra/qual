from manim import *


class SuppressedReadd(Scene):
    def construct(self):
        low = Square(z_index=0)
        high = Circle(z_index=3)
        self.add(low, high)
        self.bring_to_front(low)  # manim-lint: ignore[MLR122]
