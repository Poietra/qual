from manim import ORIGIN, RIGHT, Scene, Square


class ConditionalRegistration(Scene):
    def construct(self, condition):
        square = Square()
        if condition:
            square.add_updater(lambda mob: mob.move_to(ORIGIN))
        self.add(square)
        self.play(square.animate.shift(RIGHT))
