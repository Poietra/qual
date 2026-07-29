from manim import *


class Suppressed(VMobject):
    def cubic_curves(self):
        return self.points.reshape((-1, 4, 3))  # qual: ignore[MLR112]
