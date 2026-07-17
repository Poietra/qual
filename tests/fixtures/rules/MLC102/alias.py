import manim as mn


class Demo(mn.Scene):
    def construct(self):
        square = mn.Square()
        self.play(square.shift(mn.RIGHT))
