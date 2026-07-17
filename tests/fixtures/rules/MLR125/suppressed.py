from manim import *


class Suppressed(Scene):
    def construct(self):
        spacer = Mobject()
        self.add(spacer)  # manim-lint: ignore[MLR125]
        self.wait()
