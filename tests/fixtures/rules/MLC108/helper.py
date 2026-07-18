from manim import *


class HelperConflict(Scene):
    def spin_and_slide(self, mob):
        self.play(mob.animate.shift(RIGHT), mob.animate.rotate(PI))

    def construct(self):
        square = Square()
        self.add(square)
        self.spin_and_slide(square)
