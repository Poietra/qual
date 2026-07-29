from manim import *


class Suppressed(Scene):
    def construct(self):
        path = VMobject()
        path.set_points_as_corners([[0, 0], [1, 1]])  # qual: ignore[MLR114]
        self.add(path)
