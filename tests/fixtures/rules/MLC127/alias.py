import manim as mn


class Demo(mn.Scene):
    def construct(self):
        square = mn.Square()
        group = mn.VGroup(square, square)
