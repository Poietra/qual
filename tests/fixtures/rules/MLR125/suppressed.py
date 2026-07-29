from manim import *


class Suppressed(Scene):
    def construct(self):
        spacer = Mobject()
        self.add(spacer)  # qual: ignore[MLR125]
        self.wait()
