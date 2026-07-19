import manim as mn


class AliasSuspendedUpdaterResult(mn.Scene):
    def construct(self):
        square = mn.Square()
        square.add_updater(lambda mob: mob.move_to(mn.ORIGIN))
        self.add(square)
        self.play(square.animate.shift(mn.RIGHT))
