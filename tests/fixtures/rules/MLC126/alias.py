import manim as mn


class Demo(mn.Scene):
    def construct(self):
        image = mn.ImageMobject("photo.png")
        group = mn.VGroup(image)
